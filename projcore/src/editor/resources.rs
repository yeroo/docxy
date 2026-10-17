//! Resources one at a time, by their UID (#1588): what an agent's
//! `resource.add`, `resource.set` and `resource.del` do, and what Project's
//! Resource Sheet and Resource Information dialog edit. Each is one undo
//! step; every argument is checked before anything changes.
//!
//! Names address resources elsewhere (the Resource Names cell,
//! [`ResourceRef::Name`]), so a name is unique ignoring ASCII case, is not a
//! number and holds no comma. A resource's type prices and schedules its
//! assignments, so it changes only while it has none. A rate edit keeps
//! cost rate table A's current row in step (`assign::assignment_cost` prices
//! from the table when it has rows), and a Max. Units edit the availability
//! period in effect (the leveler takes capacity from the periods when there
//! are any); the assignments' costs and the totals refresh as after any edit
//! (`assign::refresh` treats a resource's pricing as an input).
use super::*;
use crate::model::{AccrueAt, Calendar, Rate};

/// What [`Editor::set_resource`] changes; `None` keeps a value, and an
/// inner `None` clears an optional one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourcePatch {
    pub name: Option<String>,
    pub kind: Option<ResourceType>,
    pub initials: Option<Option<String>>,
    pub group: Option<Option<String>>,
    pub code: Option<Option<String>>,
    pub email: Option<Option<String>>,
    pub material_label: Option<Option<String>>,
    /// A fraction (1.0 = 100%); a work resource's only.
    pub max_units: Option<f64>,
    /// Currency per hour (per unit for a material), as stored.
    pub standard_rate: Option<Option<Rate>>,
    pub overtime_rate: Option<Option<Rate>>,
    /// In hundredths, as stored.
    pub cost_per_use: Option<Option<Rate>>,
    pub accrue_at: Option<AccrueAt>,
    /// A base calendar's uid: the resource's own calendar derives from it,
    /// or is it.
    pub base_calendar: Option<i32>,
    pub notes: Option<Option<String>>,
}

impl ResourcePatch {
    pub fn is_empty(&self) -> bool {
        *self == ResourcePatch::default()
    }
}

/// The name a resource may take: trimmed, not empty, not a number, without
/// a comma, and no other resource's (ignoring ASCII case).
pub(super) fn checked_resource_name(
    proj: &Project,
    name: &str,
    own: Option<i32>,
) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a resource needs a name".into());
    }
    if name.parse::<f64>().is_ok() {
        return Err(format!("'{name}' is a number, not a resource name"));
    }
    if name.contains(',') {
        return Err(format!(
            "'{name}' holds a comma, which separates resource names"
        ));
    }
    if let Some(other) = proj
        .resources
        .iter()
        .find(|r| Some(r.uid) != own && r.name.eq_ignore_ascii_case(name))
    {
        return Err(format!(
            "a resource named '{}' exists (uid {})",
            other.name, other.uid
        ));
    }
    Ok(name.to_string())
}

/// The base calendar a resource's calendar derives from (or is): the
/// plan's default when it has none or an unknown one.
pub fn resource_base_calendar(proj: &Project, r: &Resource) -> Option<i32> {
    let own = r.calendar_uid.and_then(|uid| proj.calendar(uid));
    match own {
        Some(cal) => Some(cal.base_calendar_uid.unwrap_or(cal.uid)),
        None => proj
            .calendar(proj.default_calendar_uid)
            .map(|c| c.base_calendar_uid.unwrap_or(c.uid)),
    }
}

/// A rate given as a number: finite and not negative.
fn checked_rate(what: &str, rate: &Option<Rate>) -> Result<(), String> {
    if let Some(rate) = rate {
        match rate.to_f64() {
            Some(v) if v >= 0.0 => {}
            _ => {
                return Err(format!(
                    "{what} must be a number of at least 0, not {}",
                    rate.as_str()
                ));
            }
        }
    }
    Ok(())
}

fn blank_to_none(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.trim().is_empty())
}

impl Editor {
    /// The moment "now" is for a resource's dated tables: the plan's
    /// current date, else its status date, else its start.
    fn plan_now(&self) -> DateTime {
        let proj = &self.proj;
        proj.option("CurrentDate")
            .and_then(DateTime::parse_mspdi)
            .or_else(|| proj.status_date())
            .unwrap_or(self.sched.project_start)
    }

    /// Apply `patch` to `r` (a copy of the resource), checking every value.
    /// Returns whether the resource's own calendar needs rebasing.
    fn patch_resource(&self, r: &mut Resource, patch: &ResourcePatch) -> Result<(), String> {
        let proj = &self.proj;
        if let Some(name) = &patch.name {
            r.name = checked_resource_name(proj, name, Some(r.uid))?;
        }
        if let Some(kind) = patch.kind
            && kind != r.kind
        {
            let assigned = proj
                .assignments
                .iter()
                .filter(|a| a.resource_uid == r.uid)
                .count();
            if assigned > 0 {
                return Err(format!(
                    "'{}' has {assigned} assignment{} priced and scheduled by its type: remove them first",
                    r.name,
                    if assigned == 1 { "" } else { "s" }
                ));
            }
            r.kind = kind;
        }
        if let Some(v) = &patch.initials {
            r.initials = blank_to_none(v.clone());
        }
        if let Some(v) = &patch.group {
            r.group = blank_to_none(v.clone());
        }
        if let Some(v) = &patch.code {
            r.code = blank_to_none(v.clone());
        }
        if let Some(v) = &patch.email {
            r.email_address = blank_to_none(v.clone());
        }
        if let Some(v) = &patch.material_label {
            r.material_label = blank_to_none(v.clone());
        }
        if let Some(v) = &patch.notes {
            r.notes = blank_to_none(v.clone());
        }
        if let Some(units) = patch.max_units {
            if r.kind != ResourceType::Work {
                return Err(format!(
                    "'{}' is a {} resource, which has no Max. Units",
                    r.name,
                    kind_name(r.kind)
                ));
            }
            if !units.is_finite() || units < 0.0 {
                return Err(format!(
                    "max_units must be a number of at least 0, not {units}"
                ));
            }
            r.max_units = units;
            // The period in effect now follows, as Project's Resource
            // Availability table does for the Max. Units cell.
            let now = self.plan_now();
            if let Some(p) = r.availability_periods.iter_mut().find(|p| {
                p.available_from.is_none_or(|from| from <= now)
                    && p.available_to.is_none_or(|to| now < to)
            }) && p.available_units.is_some()
            {
                p.available_units = Rate::from_f64(units);
            }
        }
        let rates = [
            ("standard_rate", &patch.standard_rate),
            ("overtime_rate", &patch.overtime_rate),
            ("cost_per_use", &patch.cost_per_use),
        ];
        for (what, rate) in rates {
            if let Some(rate) = rate {
                checked_rate(what, rate)?;
            }
        }
        if rates.iter().any(|(_, rate)| rate.is_some()) {
            // Cost rate table A's row in effect now mirrors the three.
            let now = self.plan_now();
            let current = {
                let mut open = false;
                let mut rows: Vec<usize> = r
                    .rates
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.rate_table.unwrap_or(0) == 0)
                    .filter(|(_, e)| e.rates_from.is_some() || !std::mem::replace(&mut open, true))
                    .map(|(k, _)| k)
                    .collect();
                rows.sort_by_key(|&k| r.rates[k].rates_from);
                rows.iter()
                    .rev()
                    .find(|&&k| r.rates[k].rates_from.is_none_or(|from| from <= now))
                    .or(rows.first())
                    .copied()
            };
            if let Some(v) = &patch.standard_rate {
                r.standard_rate = v.clone();
                if let Some(k) = current {
                    r.rates[k].standard_rate = v.clone();
                }
            }
            if let Some(v) = &patch.overtime_rate {
                r.overtime_rate = v.clone();
                if let Some(k) = current {
                    r.rates[k].overtime_rate = v.clone();
                }
            }
            if let Some(v) = &patch.cost_per_use {
                r.cost_per_use = v.clone();
                if let Some(k) = current {
                    r.rates[k].cost_per_use = v.clone();
                }
            }
        }
        if let Some(accrue) = patch.accrue_at {
            if accrue == AccrueAt::Invalid {
                return Err("accrue_at is Start, End or Prorated".into());
            }
            r.accrue_at = Some(accrue);
        }
        if let Some(base) = patch.base_calendar {
            let is_base = proj
                .calendars
                .iter()
                .any(|c| c.uid == base && c.base_calendar_uid.is_none());
            if !is_base {
                return Err(format!(
                    "calendar {base} is not a base calendar of this plan"
                ));
            }
        }
        Ok(())
    }

    /// Point `r`'s calendar at base calendar `base`: its own derived
    /// calendar is rebased, else the base becomes its calendar.
    fn rebase_resource_calendar(proj: &mut Project, r: &mut Resource, base: i32) {
        let own = r
            .calendar_uid
            .and_then(|uid| proj.calendars.iter_mut().find(|c| c.uid == uid));
        match own {
            Some(cal) if cal.base_calendar_uid.is_some() => cal.base_calendar_uid = Some(base),
            _ => r.calendar_uid = Some(base),
        }
    }

    /// Add a resource named `name` of `kind` (100% available, costs
    /// prorated), with `patch`'s other values, as one undo step; returns its
    /// UID.
    pub fn add_resource(
        &mut self,
        name: &str,
        kind: ResourceType,
        patch: &ResourcePatch,
    ) -> Result<i32, String> {
        let name = checked_resource_name(&self.proj, name, None)?;
        let uid = self
            .proj
            .resources
            .iter()
            .map(|r| r.uid)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("No resource IDs available")?;
        let id = self
            .proj
            .resources
            .iter()
            .map(|r| r.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("No resource IDs available")?;
        let mut r = Resource {
            uid,
            id,
            name,
            kind,
            max_units: 1.0,
            accrue_at: Some(AccrueAt::Prorated),
            ..Resource::default()
        };
        let patch = ResourcePatch {
            name: None,
            kind: None,
            ..patch.clone()
        };
        self.patch_resource(&mut r, &patch)?;
        self.snapshot();
        if let Some(base) = patch.base_calendar {
            Self::rebase_resource_calendar(&mut self.proj, &mut r, base);
        }
        self.proj.resources.push(r);
        self.changed();
        Ok(uid)
    }

    /// Change resource `uid` by `patch` as one undo step; `Ok(false)` when
    /// nothing differs. A rejected value changes nothing.
    pub fn set_resource(&mut self, uid: i32, patch: &ResourcePatch) -> Result<bool, String> {
        if patch.is_empty() {
            return Err("nothing to change".into());
        }
        let k = self
            .proj
            .resources
            .iter()
            .position(|r| r.uid == uid)
            .ok_or_else(|| format!("no resource with uid {uid}"))?;
        let mut r = self.proj.resources[k].clone();
        self.patch_resource(&mut r, patch)?;
        let mut next = self.proj.clone();
        if let Some(base) = patch.base_calendar {
            Self::rebase_resource_calendar(&mut next, &mut r, base);
        }
        next.resources[k] = r;
        if next == self.proj {
            return Ok(false);
        }
        if let Some(error) = crate::schedule::calendar_error(&next) {
            return Err(error);
        }
        let prev = std::mem::replace(&mut self.proj, next);
        self.push_undo(prev);
        self.changed();
        Ok(true)
    }

    /// Delete resource `uid` with its assignments (each removed as the
    /// Resource Names cell removes a name, so its task follows its type)
    /// and its own derived calendar when nothing else uses it, as one undo
    /// step; the resources after it move up one ID. Returns the removed
    /// assignments' UIDs.
    pub fn delete_resource(&mut self, uid: i32) -> Result<Vec<i32>, String> {
        let r = self
            .proj
            .resources
            .iter()
            .find(|r| r.uid == uid)
            .ok_or_else(|| format!("no resource with uid {uid}"))?;
        let (id, calendar) = (r.id, r.calendar_uid);
        let assignments: Vec<i32> = self
            .proj
            .assignments
            .iter()
            .filter(|a| a.resource_uid == uid)
            .map(|a| a.uid)
            .collect();
        self.batch(|ed| {
            for a in &assignments {
                ed.delete_assignment(*a)?;
            }
            ed.snapshot();
            let proj = &mut ed.proj;
            proj.resources.retain(|r| r.uid != uid);
            for r in &mut proj.resources {
                if r.id > id {
                    r.id -= 1;
                }
            }
            if let Some(cal) = calendar {
                let derived = proj
                    .calendar(cal)
                    .is_some_and(|c| c.base_calendar_uid.is_some());
                let used = proj.resources.iter().any(|r| r.calendar_uid == Some(cal))
                    || proj.tasks.iter().any(|t| t.calendar_uid == Some(cal))
                    || proj
                        .calendars
                        .iter()
                        .any(|c| c.base_calendar_uid == Some(cal))
                    || proj.default_calendar_uid == cal;
                if derived && !used {
                    proj.calendars.retain(|c: &Calendar| c.uid != cal);
                }
            }
            ed.changed();
            Ok(())
        })?;
        Ok(assignments)
    }
}

/// `work`, `material` or `cost`.
pub fn kind_name(kind: ResourceType) -> &'static str {
    match kind {
        ResourceType::Work => "work",
        ResourceType::Material => "material",
        ResourceType::Cost => "cost",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AvailabilityPeriod, RateEntry};

    fn editor() -> Editor {
        let mut proj = untitled_project();
        proj.tasks.push(Task {
            uid: 1,
            id: 1,
            name: "Build".into(),
            outline_level: 1,
            duration_min: 5 * 480,
            work_min: Some(0),
            cost: Rate::parse("0"),
            ..Task::default()
        });
        let standard = proj.default_calendar_uid;
        proj.calendars.push(Calendar {
            uid: 7,
            name: "Night".into(),
            ..Calendar::standard(7)
        });
        proj.calendars.push(Calendar {
            uid: 8,
            name: "Ann".into(),
            base_calendar_uid: Some(standard),
            ..Calendar::standard(8)
        });
        proj.resources.push(Resource {
            uid: 1,
            id: 1,
            name: "Ann".into(),
            max_units: 1.0,
            standard_rate: Rate::parse("50"),
            calendar_uid: Some(8),
            rates: vec![RateEntry {
                rate_table: Some(0),
                standard_rate: Rate::parse("50"),
                ..RateEntry::default()
            }],
            availability_periods: vec![AvailabilityPeriod {
                available_units: Rate::parse("1"),
                ..AvailabilityPeriod::default()
            }],
            ..Resource::default()
        });
        Editor::new(proj)
    }

    fn resource(ed: &Editor, uid: i32) -> &Resource {
        ed.project()
            .resources
            .iter()
            .find(|r| r.uid == uid)
            .unwrap()
    }

    #[test]
    fn a_resource_is_added_with_a_checked_name_and_its_values() {
        let mut ed = editor();
        for bad in ["", "  ", "12", "Ann", "ANN", "Ann, Bob"] {
            assert!(
                ed.add_resource(bad, ResourceType::Work, &ResourcePatch::default())
                    .is_err(),
                "{bad:?}"
            );
        }
        assert_eq!(ed.undo_depth(), 0);
        let patch = ResourcePatch {
            initials: Some(Some("B".into())),
            group: Some(Some("Crew".into())),
            max_units: Some(0.5),
            standard_rate: Some(Rate::parse("30")),
            cost_per_use: Some(Rate::parse("1000")),
            accrue_at: Some(AccrueAt::End),
            base_calendar: Some(7),
            notes: Some(Some("Part time".into())),
            ..ResourcePatch::default()
        };
        let uid = ed
            .add_resource(" Bob ", ResourceType::Work, &patch)
            .unwrap();
        assert_eq!(uid, 2);
        let bob = resource(&ed, uid);
        assert_eq!((bob.id, bob.name.as_str()), (2, "Bob"));
        assert_eq!(bob.initials.as_deref(), Some("B"));
        assert_eq!(bob.max_units, 0.5);
        assert_eq!(bob.standard_rate.as_ref().map(Rate::as_str), Some("30"));
        assert_eq!(bob.cost_per_use.as_ref().map(Rate::as_str), Some("1000"));
        assert_eq!(bob.accrue_at, Some(AccrueAt::End));
        assert_eq!(bob.calendar_uid, Some(7));
        assert_eq!(bob.notes.as_deref(), Some("Part time"));
        assert_eq!(ed.undo_depth(), 1);
        // A material has no Max. Units; a bad base calendar is refused.
        let steel = ResourcePatch {
            max_units: Some(1.0),
            ..ResourcePatch::default()
        };
        assert!(
            ed.add_resource("Steel", ResourceType::Material, &steel)
                .is_err()
        );
        let moon = ResourcePatch {
            base_calendar: Some(99),
            ..ResourcePatch::default()
        };
        assert!(ed.add_resource("Steel", ResourceType::Work, &moon).is_err());
        assert!(
            ed.add_resource("Steel", ResourceType::Material, &ResourcePatch::default())
                .is_ok()
        );
        assert_eq!(resource(&ed, 3).kind, ResourceType::Material);
        assert_eq!(resource(&ed, 3).accrue_at, Some(AccrueAt::Prorated));
        assert!(ed.undo());
        assert!(ed.undo());
        assert_eq!(ed.project().resources.len(), 1);
    }

    #[test]
    fn a_resource_edit_keeps_its_rate_table_and_availability_in_step() {
        let mut ed = editor();
        ed.add_assignment(1, ResourceRef::Uid(1), None, None)
            .unwrap();
        let depth = ed.undo_depth();
        let patch = ResourcePatch {
            standard_rate: Some(Rate::parse("60")),
            overtime_rate: Some(Rate::parse("90")),
            max_units: Some(2.0),
            ..ResourcePatch::default()
        };
        assert_eq!(ed.set_resource(1, &patch), Ok(true));
        let ann = resource(&ed, 1);
        assert_eq!(ann.standard_rate.as_ref().map(Rate::as_str), Some("60"));
        assert_eq!(
            ann.rates[0].standard_rate.as_ref().map(Rate::as_str),
            Some("60")
        );
        assert_eq!(
            ann.rates[0].overtime_rate.as_ref().map(Rate::as_str),
            Some("90")
        );
        assert_eq!(
            ann.availability_periods[0]
                .available_units
                .as_ref()
                .map(Rate::as_str),
            Some("2")
        );
        // The assignment's cost follows the new rate: 40 h at $60.
        let a = &ed.project().assignments[0];
        assert_eq!(a.cost.as_ref().map(Rate::as_str), Some("240000"));
        assert_eq!(ed.undo_depth(), depth + 1);
        // Unchanged values are no step; refusals change nothing.
        assert_eq!(ed.set_resource(1, &patch), Ok(false));
        assert!(ed.set_resource(1, &ResourcePatch::default()).is_err());
        assert!(ed.set_resource(99, &patch).is_err());
        let before = ed.project().clone();
        for bad in [
            ResourcePatch {
                kind: Some(ResourceType::Material),
                ..ResourcePatch::default()
            },
            ResourcePatch {
                standard_rate: Some(Rate::parse("-1")),
                ..ResourcePatch::default()
            },
            ResourcePatch {
                max_units: Some(f64::NAN),
                ..ResourcePatch::default()
            },
            ResourcePatch {
                name: Some("7".into()),
                ..ResourcePatch::default()
            },
            ResourcePatch {
                accrue_at: Some(AccrueAt::Invalid),
                ..ResourcePatch::default()
            },
        ] {
            assert!(ed.set_resource(1, &bad).is_err(), "{bad:?}");
        }
        assert_eq!(*ed.project(), before);
        assert_eq!(ed.undo_depth(), depth + 1);
        // A rename keeps the case of the new spelling; the own name is free.
        let rename = ResourcePatch {
            name: Some("ann".into()),
            ..ResourcePatch::default()
        };
        assert_eq!(ed.set_resource(1, &rename), Ok(true));
        assert_eq!(resource(&ed, 1).name, "ann");
        // Rebasing a derived calendar edits the calendar, not the resource.
        let night = ResourcePatch {
            base_calendar: Some(7),
            ..ResourcePatch::default()
        };
        assert_eq!(ed.set_resource(1, &night), Ok(true));
        assert_eq!(resource(&ed, 1).calendar_uid, Some(8));
        assert_eq!(ed.project().calendar(8).unwrap().base_calendar_uid, Some(7));
        assert_eq!(
            resource_base_calendar(ed.project(), resource(&ed, 1)),
            Some(7)
        );
        assert!(ed.undo());
        assert_eq!(
            resource_base_calendar(ed.project(), resource(&ed, 1)),
            Some(before.default_calendar_uid)
        );
    }

    #[test]
    fn deleting_a_resource_takes_its_assignments_and_calendar_as_one_step() {
        let mut ed = editor();
        let bob = ed
            .add_resource("Bob", ResourceType::Work, &ResourcePatch::default())
            .unwrap();
        let a1 = ed
            .add_assignment(1, ResourceRef::Uid(1), None, None)
            .unwrap();
        let a2 = ed
            .add_assignment(1, ResourceRef::Uid(bob), None, None)
            .unwrap();
        let depth = ed.undo_depth();
        assert_eq!(ed.delete_resource(1), Ok(vec![a1]));
        assert_eq!(ed.undo_depth(), depth + 1);
        let proj = ed.project();
        assert_eq!(proj.resources.len(), 1);
        assert_eq!((proj.resources[0].uid, proj.resources[0].id), (bob, 1));
        assert_eq!(
            proj.assignments.iter().map(|a| a.uid).collect::<Vec<_>>(),
            [a2]
        );
        // Ann's own calendar went with her; the base calendars stay.
        assert!(proj.calendar(8).is_none());
        assert!(proj.calendar(7).is_some());
        assert!(ed.delete_resource(1).is_err());
        assert!(ed.undo());
        let proj = ed.project();
        assert_eq!(proj.resources.len(), 2);
        assert_eq!(proj.assignments.len(), 2);
        assert!(proj.calendar(8).is_some());
    }
}
