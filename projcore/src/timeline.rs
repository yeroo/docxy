//! Which tasks show on the Timeline (Task Information's Display on
//! Timeline, #1536). MSPDI has no element for it, so it is saved only in the
//! `.yppx` package's `timeline.xml` part ([`crate::yppx`]), one `<task>` per
//! task that states a value; the task's [`crate::model::Task::display_on_timeline`]
//! holds it in the model. The flag is only stored: the Timeline pane does not
//! read it yet.

use crate::flagpart::TaskFlagPart;
use crate::model::Task;

/// The part's description ([`TaskFlagPart`]).
pub const FLAGS: TaskFlagPart = TaskFlagPart {
    part: PART,
    content_type: CONTENT_TYPE,
    root: "timeline",
    attr: "show",
    get: |t| t.display_on_timeline,
    set: |t, on| t.display_on_timeline = Some(on),
};

/// The `.yppx` part the flags are saved in.
pub const PART: &str = "timeline.xml";
/// Its content type in `[Content_Types].xml`.
pub const CONTENT_TYPE: &str = "application/vnd.yppx.timeline+xml";

/// Whether any task states a value, so the part is written at all.
pub fn any(tasks: &[Task]) -> bool {
    FLAGS.any(tasks)
}

/// The `timeline.xml` part.
pub fn to_xml(tasks: &[Task]) -> String {
    FLAGS.to_xml(tasks)
}

/// Read a `timeline.xml` part into (uid, shown) pairs. Unknown elements are
/// ignored and a `<task>` without a readable uid or show is skipped.
pub fn from_xml(xml: &str) -> Result<Vec<(i32, bool)>, String> {
    FLAGS.from_xml(xml)
}

/// Put read flags on their tasks; a uid no task has is dropped.
pub fn apply(tasks: &mut [Task], flags: &[(i32, bool)]) {
    FLAGS.apply(tasks, flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(uid: i32, on: Option<bool>) -> Task {
        Task {
            uid,
            display_on_timeline: on,
            ..Task::default()
        }
    }

    #[test]
    fn xml_round_trips_and_skips_unstated_tasks() {
        let tasks = vec![task(1, None), task(2, Some(true)), task(3, Some(false))];
        assert!(any(&tasks) && !any(&tasks[..1]));
        let flags = from_xml(&to_xml(&tasks)).unwrap();
        assert_eq!(flags, vec![(2, true), (3, false)]);
        let mut back = vec![task(1, None), task(2, None), task(3, None)];
        apply(&mut back, &flags);
        assert_eq!(back, tasks);
    }

    #[test]
    fn reads_later_builds_and_refuses_the_wrong_root() {
        let got = from_xml(
            "<timeline v=\"2\"><task uid=\"4\" show=\"1\" x=\"y\"/>\
             <task uid=\"x\" show=\"1\"/><task uid=\"5\" show=\"maybe\"/><other/></timeline>",
        )
        .unwrap();
        assert_eq!(got, vec![(4, true)]);
        assert!(from_xml("<tableview/>").is_err());
        assert!(from_xml("<timeline><task").is_err());
    }
}
