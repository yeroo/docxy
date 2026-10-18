//! The Resources page's grid (#1536): Resource Name, Assignment Owner, Units
//! and Cost, one row per assignment and a blank row for the next. The owner
//! and the cost are read-only (label columns). OK turns the rows into the
//! Resource Names cell's tokens (`Ann`, `Bo[50%]`, `Cement[5 tons]`) and
//! applies them through `Editor::set_resources`, so a row left as shown
//! changes nothing and a new name makes a resource, as in the cell.
use super::*;
use projcore::editor::{AssignmentField, read_assignment_field};
use projcore::model::ResourceType;

const COLUMNS: [&str; 4] = ["Resource Name", "Assignment Owner", "Units", "Cost"];

const NAME: usize = 0;
const OWNER: usize = 1;
const UNITS: usize = 2;

/// A resource's kind as a letter, kept beside the names it belongs to.
fn kind_letter(kind: ResourceType) -> &'static str {
    match kind {
        ResourceType::Work => "w",
        ResourceType::Material => "m",
        ResourceType::Cost => "c",
    }
}

/// The grid for `task`'s assignments.
pub(super) fn grid(ed: &ProjectEditor, task: &Task) -> Control {
    let proj = ed.project();
    let mut rows: Vec<Vec<String>> = proj
        .assignments
        .iter()
        .filter(|a| a.task_uid == task.uid)
        .filter_map(|a| {
            let r = proj.resources.iter().find(|r| r.uid == a.resource_uid)?;
            let read = |f| read_assignment_field(ed, a, f).text;
            Some(vec![
                r.name.clone(),
                a.assn_owner.clone().unwrap_or_default(),
                read(AssignmentField::Units),
                read(AssignmentField::Cost),
            ])
        })
        .collect();
    rows.push(vec![String::new(); COLUMNS.len()]);
    let known: Vec<_> = proj
        .resources
        .iter()
        .filter(|r| !r.name.is_empty())
        .collect();
    let mut c = Control::new(
        "resources",
        "&Resources:",
        ControlKind::Grid,
        Value::Rows(rows),
    );
    c.page = Some(2);
    c.columns = COLUMNS.map(String::from).to_vec();
    c.column_kinds = vec![
        ControlKind::Combo,
        ControlKind::Label,
        ControlKind::Text,
        ControlKind::Label,
    ];
    // The plan's resources, and (in the owner column, which offers nothing)
    // their kinds, item for item.
    let mut items = vec![Vec::new(); COLUMNS.len()];
    items[NAME] = known.iter().map(|r| r.name.clone()).collect();
    items[OWNER] = known
        .iter()
        .map(|r| kind_letter(r.kind).to_string())
        .collect();
    c.column_items = items;
    c.editable_grid = true;
    c
}

/// A cell committed: a name typed into a row with no units starts it at 100%
/// (a work resource's usual), a material or cost resource at none.
pub(super) fn follow(d: &mut Dialog, i: usize, before: &Value) {
    let (Value::Rows(old), Value::Rows(new)) = (before, d.controls[i].value.clone()) else {
        return;
    };
    let Some(r) =
        (0..new.len()).find(|&r| old.get(r).map(|o| o.get(NAME)) != Some(new[r].get(NAME)))
    else {
        return;
    };
    let names = d.controls[i].column_items[NAME].clone();
    let kinds = d.controls[i].column_items[OWNER].clone();
    let Value::Rows(rows) = &mut d.controls[i].value else {
        return;
    };
    let row = &mut rows[r];
    let want = row[NAME].trim().to_lowercase();
    let hit = names.iter().position(|n| n.to_lowercase() == want);
    if let Some(k) = hit {
        row[NAME] = names[k].clone();
    }
    let work = hit.is_none_or(|k| kinds[k] == "w");
    if !want.is_empty() && row[UNITS].trim().is_empty() && work {
        row[UNITS] = "100%".into();
    }
}

/// The Resource Names tokens the rows say, for `set_resources`. A row with no
/// name is ignored. A work resource's units are a percent (a bare number
/// takes the `%`), 100% or none being the bare name; a material's are its
/// quantity and label as the cell spells them.
pub(super) fn tokens(ed: &ProjectEditor, rows: &[Vec<String>]) -> Vec<String> {
    let proj = ed.project();
    rows.iter()
        .filter_map(|row| {
            let name = row.get(NAME)?.trim();
            if name.is_empty() {
                return None;
            }
            let units = row.get(UNITS).map_or("", |u| u.trim());
            let kind = proj
                .resources
                .iter()
                .find(|r| r.name.eq_ignore_ascii_case(name))
                .map_or(ResourceType::Work, |r| r.kind);
            let units = if kind == ResourceType::Material {
                units.to_string()
            } else if units.is_empty() || units == "100%" {
                String::new()
            } else if units.parse::<f64>().is_ok() {
                format!("{units}%")
            } else {
                units.to_string()
            };
            Some(if units.is_empty() {
                name.to_string()
            } else {
                format!("{name}[{units}]")
            })
        })
        .collect()
}
