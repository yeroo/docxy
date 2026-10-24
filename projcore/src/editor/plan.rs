//! Plan-level settings as Project's Project Information dialog edits them
//! (#1587): the title, the start date, the status and current dates, the
//! default calendar. Each is one undo step; the start and the calendar
//! reschedule.
use super::*;

impl Editor {
    /// Set the plan's title (MSPDI `Title`), trimmed; `Ok(false)` when
    /// unchanged.
    pub fn set_title(&mut self, title: &str) -> Result<bool, String> {
        let title = title.trim();
        if self.proj.title == title {
            return Ok(false);
        }
        self.snapshot();
        self.proj.title = title.to_string();
        self.changed();
        Ok(true)
    }

    /// Set the plan's start date (MSPDI `StartDate`), the anchor every
    /// unlinked, unconstrained task starts at; `Ok(false)` when unchanged.
    pub fn set_project_start(&mut self, start: DateTime) -> Result<bool, String> {
        if self.proj.start_date == Some(start) {
            return Ok(false);
        }
        self.snapshot();
        self.proj.start_date = Some(start);
        self.changed();
        Ok(true)
    }

    /// Set or clear the status date (MSPDI `StatusDate`), what Mark on
    /// Track and the Status field measure against; `Ok(false)` when
    /// unchanged.
    pub fn set_status_date(&mut self, date: Option<DateTime>) -> Result<bool, String> {
        self.set_dated_option("StatusDate", date)
    }

    /// Set or clear the current date (MSPDI `CurrentDate`), the chart's
    /// current date line and the status date's stand-in; `Ok(false)` when
    /// unchanged.
    pub fn set_current_date(&mut self, date: Option<DateTime>) -> Result<bool, String> {
        self.set_dated_option("CurrentDate", date)
    }

    fn set_dated_option(&mut self, name: &str, date: Option<DateTime>) -> Result<bool, String> {
        let current = self.proj.option(name).and_then(DateTime::parse_mspdi);
        if current == date && (date.is_some() || self.proj.option(name).is_none()) {
            return Ok(false);
        }
        self.snapshot();
        self.proj
            .set_option(name, date.map(DateTime::to_mspdi).as_deref());
        self.changed();
        Ok(true)
    }

    /// Set the plan's default calendar (MSPDI `CalendarUID`) to a base
    /// calendar of the plan; `Ok(false)` when unchanged.
    pub fn set_default_calendar(&mut self, uid: i32) -> Result<bool, String> {
        let base = self
            .proj
            .calendars
            .iter()
            .any(|c| c.uid == uid && c.base_calendar_uid.is_none());
        if !base {
            return Err(format!(
                "calendar {uid} is not a base calendar of this plan"
            ));
        }
        if self.proj.default_calendar_uid == uid {
            return Ok(false);
        }
        // A calendar without working time cannot be the plan's (#1605).
        let mut next = self.proj.clone();
        next.default_calendar_uid = uid;
        if let Some(error) = crate::schedule::calendar_error(&next) {
            return Err(error);
        }
        self.capture_view();
        let prev = std::mem::replace(&mut self.proj, next);
        self.push_undo(prev);
        self.changed();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Calendar;

    fn editor() -> Editor {
        let mut proj = untitled_project();
        proj.tasks.push(Task {
            uid: 1,
            id: 1,
            name: "A".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        });
        Editor::new(proj)
    }

    #[test]
    fn the_plan_settings_are_undo_steps_and_unchanged_values_are_not() {
        let mut ed = editor();
        let depth = ed.undo_depth();
        assert_eq!(ed.set_title("  Build  "), Ok(true));
        assert_eq!(ed.project().title, "Build");
        assert_eq!(ed.set_title("Build"), Ok(false));
        let start = DateTime::from_ymd_hm(2026, 3, 9, 8, 0);
        assert_eq!(ed.set_project_start(start), Ok(true));
        assert_eq!(ed.project().start_date, Some(start));
        assert_eq!(ed.disp_start(1), Some(start));
        assert_eq!(ed.set_project_start(start), Ok(false));
        let status = DateTime::from_ymd_hm(2026, 3, 16, 17, 0);
        assert_eq!(ed.set_status_date(Some(status)), Ok(true));
        assert_eq!(ed.project().status_date(), Some(status));
        assert_eq!(
            ed.project().option("StatusDate"),
            Some("2026-03-16T17:00:00")
        );
        assert_eq!(ed.set_status_date(Some(status)), Ok(false));
        assert_eq!(ed.set_current_date(Some(start)), Ok(true));
        assert_eq!(
            ed.project().option("CurrentDate"),
            Some("2026-03-09T08:00:00")
        );
        assert_eq!(ed.set_status_date(None), Ok(true));
        assert_eq!(ed.project().option("StatusDate"), None);
        assert_eq!(ed.set_status_date(None), Ok(false));
        // Without a status date the current date stands in.
        assert_eq!(ed.project().status_date(), Some(start));
        assert_eq!(ed.undo_depth() - depth, 5);
        assert!(ed.undo());
        assert_eq!(
            ed.project().option("StatusDate"),
            Some("2026-03-16T17:00:00")
        );
    }

    #[test]
    fn a_status_date_edit_moves_the_late_status_at_once() {
        // The suite's Late Tasks text style reads this per frame (#1665).
        let mut ed = editor();
        let start = DateTime::from_ymd_hm(2026, 3, 9, 8, 0);
        ed.set_project_start(start).unwrap();
        let status = |ed: &Editor| {
            let task = ed.project().task(1).unwrap().clone();
            crate::editor::FieldReader::new(ed)
                .read(&task, crate::editor::Field::Status)
                .text
        };
        ed.set_status_date(Some(DateTime::from_ymd_hm(2026, 3, 6, 17, 0)))
            .unwrap();
        assert_eq!(status(&ed), "Future Task");
        ed.set_status_date(Some(DateTime::from_ymd_hm(2026, 3, 16, 17, 0)))
            .unwrap();
        assert_eq!(status(&ed), "Late");
    }

    #[test]
    fn the_default_calendar_must_be_a_base_calendar_of_the_plan() {
        let mut ed = editor();
        let standard = ed.project().default_calendar_uid;
        assert_eq!(ed.set_default_calendar(standard), Ok(false));
        assert!(ed.set_default_calendar(99).is_err());
        let mut proj = ed.project().clone();
        proj.calendars.push(Calendar {
            uid: 7,
            name: "Night".into(),
            ..Calendar::standard(7)
        });
        proj.calendars.push(Calendar {
            uid: 8,
            name: "Derived".into(),
            base_calendar_uid: Some(standard),
            ..Calendar::standard(8)
        });
        let mut ed = Editor::new(proj);
        assert_eq!(ed.set_default_calendar(7), Ok(true));
        assert_eq!(ed.project().default_calendar_uid, 7);
        assert!(ed.set_default_calendar(8).is_err());
        assert!(ed.undo());
        assert_eq!(ed.project().default_calendar_uid, standard);
    }
}
