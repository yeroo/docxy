//! Which tasks show on the Timeline (Task Information's Display on
//! Timeline, #1536). MSPDI has no element for it, so it is saved only in the
//! `.yppx` package's `timeline.xml` part ([`crate::yppx`]), one `<task>` per
//! task that states a value; the task's [`crate::model::Task::display_on_timeline`]
//! holds it in the model. The flag is only stored: the Timeline pane does not
//! read it yet.

use crate::flagpart::TaskFlagPart;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Task;

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
        assert!(FLAGS.any(&tasks) && !FLAGS.any(&tasks[..1]));
        let flags = FLAGS.from_xml(&FLAGS.to_xml(&tasks)).unwrap();
        assert_eq!(flags, vec![(2, true), (3, false)]);
        let mut back = vec![task(1, None), task(2, None), task(3, None)];
        FLAGS.apply(&mut back, &flags);
        assert_eq!(back, tasks);
    }

    #[test]
    fn reads_later_builds_and_refuses_the_wrong_root() {
        let got = FLAGS
            .from_xml(
                "<timeline v=\"2\"><task uid=\"4\" show=\"1\" x=\"y\"/>\
             <task uid=\"x\" show=\"1\"/><task uid=\"5\" show=\"maybe\"/><other/></timeline>",
            )
            .unwrap();
        assert_eq!(got, vec![(4, true)]);
        assert!(FLAGS.from_xml("<tableview/>").is_err());
        assert!(FLAGS.from_xml("<timeline><task").is_err());
    }
}
