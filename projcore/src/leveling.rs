//! Leveling as a plan edit (#1594): Project's Leveling Options as plan
//! options, the targets Resource > Level offers (the whole plan, a selection,
//! a resource) and what a pass reports. The engine is
//! [`crate::schedule::level_with`]; the edit, which writes each task's
//! `LevelingDelay` as one undo step, is `Editor::level_tasks` in
//! `editor/leveling.rs`. The display overlay (`Editor::leveled`, #1589)
//! stays for the display-only mode.

use crate::json::Json;
use crate::model::Project;

/// Project's "Leveling order".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LevelOrder {
    /// Among the tasks ready to place, the lowest ID first.
    IdOnly,
    /// Project's default: dependencies first, then the plan's row order
    /// (docxy's topological order; Project also weighs slack, dates,
    /// priority and constraints, unmeasured here).
    #[default]
    Standard,
    /// The highest priority first, then as Standard.
    PriorityStandard,
}

impl LevelOrder {
    pub fn name(self) -> &'static str {
        match self {
            LevelOrder::IdOnly => "ID",
            LevelOrder::Standard => "Standard",
            LevelOrder::PriorityStandard => "Priority",
        }
    }

    /// An order by name (`ID`, `Standard`, `Priority` or
    /// `Priority, Standard`; ASCII case ignored).
    pub fn parse(s: &str) -> Option<LevelOrder> {
        let s = s.trim().to_ascii_lowercase();
        Some(match s.as_str() {
            "id" | "id only" => LevelOrder::IdOnly,
            "standard" => LevelOrder::Standard,
            "priority" | "priority, standard" | "priority/standard" => LevelOrder::PriorityStandard,
            _ => return None,
        })
    }
}

/// The plan option each setting is stored under (MSPDI has no element for
/// them; they are written after the header like any other option).
const ORDER: &str = "LevelingOrder";
const WITHIN_SLACK: &str = "LevelWithinSlack";
const ADJUST: &str = "LevelAdjustAssignments";
const SPLIT: &str = "LevelCanSplit";
const CLEAR_FIRST: &str = "LevelClearBeforeLeveling";

/// Project's Leveling Options dialog, the settings a leveling pass reads. A
/// setting at its default is not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelOptions {
    pub order: LevelOrder,
    /// Level only within available slack: a task is never delayed past its
    /// total slack (left overallocated instead).
    pub within_slack: bool,
    /// Leveling can adjust individual assignments on a task (kept as the
    /// plan's setting; the leveler delays whole tasks).
    pub adjust_assignments: bool,
    /// Leveling can create splits in remaining work (kept as the plan's
    /// setting; the leveler never splits).
    pub can_split: bool,
    /// Clear leveling values before leveling: a pass starts from the dates
    /// without the tasks' stored delays.
    pub clear_first: bool,
}

impl Default for LevelOptions {
    fn default() -> Self {
        LevelOptions {
            order: LevelOrder::Standard,
            within_slack: false,
            adjust_assignments: true,
            can_split: true,
            clear_first: true,
        }
    }
}

fn flag(proj: &Project, name: &str, default: bool) -> bool {
    match proj.option(name).map(str::trim) {
        Some("1") | Some("true") => true,
        Some("0") | Some("false") => false,
        _ => default,
    }
}

impl LevelOptions {
    /// The plan's settings.
    pub fn of(proj: &Project) -> LevelOptions {
        let d = LevelOptions::default();
        LevelOptions {
            order: proj
                .option(ORDER)
                .and_then(LevelOrder::parse)
                .unwrap_or(d.order),
            within_slack: flag(proj, WITHIN_SLACK, d.within_slack),
            adjust_assignments: flag(proj, ADJUST, d.adjust_assignments),
            can_split: flag(proj, SPLIT, d.can_split),
            clear_first: flag(proj, CLEAR_FIRST, d.clear_first),
        }
    }

    /// Write the settings to the plan's options, a default clearing its
    /// option.
    pub fn store(&self, proj: &mut Project) {
        let d = LevelOptions::default();
        let bit = |on: bool, default: bool| (on != default).then_some(if on { "1" } else { "0" });
        proj.set_option(ORDER, (self.order != d.order).then_some(self.order.name()));
        proj.set_option(WITHIN_SLACK, bit(self.within_slack, d.within_slack));
        proj.set_option(ADJUST, bit(self.adjust_assignments, d.adjust_assignments));
        proj.set_option(SPLIT, bit(self.can_split, d.can_split));
        proj.set_option(CLEAR_FIRST, bit(self.clear_first, d.clear_first));
    }

    /// As the control verbs report them.
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("order", Json::str(self.order.name())),
            ("within_slack", Json::Bool(self.within_slack)),
            ("adjust_assignments", Json::Bool(self.adjust_assignments)),
            ("split", Json::Bool(self.can_split)),
            ("clear_first", Json::Bool(self.clear_first)),
        ])
    }

    /// The settings changed by a verb's arguments (`order`, `within_slack`,
    /// `adjust_assignments`, `split`, `clear_first`; a key absent keeps the
    /// setting). `at` names the verb for the messages.
    pub fn updated(&self, args: &Json, at: &str) -> Result<LevelOptions, String> {
        let mut out = *self;
        match args.get("order") {
            None | Some(Json::Null) => {}
            Some(Json::Str(s)) => {
                out.order = LevelOrder::parse(s)
                    .ok_or_else(|| format!("{at}: order '{s}' is not ID, Standard or Priority"))?;
            }
            Some(_) => return Err(format!("{at}: order is not a string")),
        }
        let flag = |key: &str, into: &mut bool| -> Result<(), String> {
            match args.get(key) {
                None | Some(Json::Null) => Ok(()),
                Some(Json::Bool(b)) => {
                    *into = *b;
                    Ok(())
                }
                Some(_) => Err(format!("{at}: {key} is not a boolean")),
            }
        };
        flag("within_slack", &mut out.within_slack)?;
        flag("adjust_assignments", &mut out.adjust_assignments)?;
        flag("split", &mut out.can_split)?;
        flag("clear_first", &mut out.clear_first)?;
        Ok(out)
    }
}

/// What a leveling pass may move: Resource > Level's three commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LevelTarget {
    /// Level All: every task.
    All,
    /// Level Selection: these tasks (a summary stands for its subtasks).
    Selection(Vec<i32>),
    /// Level Resource: the tasks this resource is assigned to.
    Resource(i32),
}

/// What a leveling pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LevelOutcome {
    /// Tasks whose stored delay or pre-leveled dates changed.
    pub changed: usize,
    /// Tasks the pass left with a delay.
    pub delayed: usize,
    /// Work resources still booked past their capacity afterwards.
    pub overallocated: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_round_trip_through_the_plan_and_defaults_store_nothing() {
        let mut proj = Project::default();
        let d = LevelOptions::of(&proj);
        assert_eq!(d, LevelOptions::default());
        d.store(&mut proj);
        assert!(proj.options.is_empty(), "{:?}", proj.options);
        let set = LevelOptions {
            order: LevelOrder::PriorityStandard,
            within_slack: true,
            adjust_assignments: false,
            can_split: false,
            clear_first: false,
        };
        set.store(&mut proj);
        assert_eq!(proj.option("LevelingOrder"), Some("Priority"));
        assert_eq!(proj.option("LevelWithinSlack"), Some("1"));
        assert_eq!(proj.option("LevelClearBeforeLeveling"), Some("0"));
        assert_eq!(LevelOptions::of(&proj), set);
        LevelOptions::default().store(&mut proj);
        assert!(proj.options.is_empty());
        for o in [
            LevelOrder::IdOnly,
            LevelOrder::Standard,
            LevelOrder::PriorityStandard,
        ] {
            assert_eq!(LevelOrder::parse(o.name()), Some(o));
        }
        assert_eq!(
            LevelOrder::parse("priority, standard"),
            Some(LevelOrder::PriorityStandard)
        );
        assert_eq!(LevelOrder::parse("slack"), None);
    }

    #[test]
    fn a_verbs_arguments_update_the_options_and_bad_ones_are_named() {
        let d = LevelOptions::default();
        let got = d
            .updated(
                &Json::parse(r#"{"order":"id","within_slack":true,"split":false}"#).unwrap(),
                "level.options",
            )
            .unwrap();
        assert_eq!(got.order, LevelOrder::IdOnly);
        assert!(got.within_slack && !got.can_split && got.clear_first);
        assert_eq!(got.to_json().text("order"), "ID");
        assert_eq!(got.to_json().get("split"), Some(&Json::Bool(false)));
        assert_eq!(
            d.updated(
                &Json::parse(r#"{"order":"slack"}"#).unwrap(),
                "level.options"
            )
            .unwrap_err(),
            "level.options: order 'slack' is not ID, Standard or Priority"
        );
        assert_eq!(
            d.updated(
                &Json::parse(r#"{"clear_first":1}"#).unwrap(),
                "level.options"
            )
            .unwrap_err(),
            "level.options: clear_first is not a boolean"
        );
        assert_eq!(d.updated(&Json::Obj(Vec::new()), "x").unwrap(), d);
    }
}
