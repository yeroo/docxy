//! Project's task Marked flag (#1667). MSPDI has no element for it, so it is
//! saved only in the `.yppx` package's `marked.xml` part ([`crate::yppx`]),
//! one `<task>` per task that states a value, the same way as
//! [`crate::timeline`]; [`crate::model::Task::marked`] holds it in the model.

use crate::flagpart::TaskFlagPart;

/// The `.yppx` part the flags are saved in.
pub const PART: &str = "marked.xml";
/// Its content type in `[Content_Types].xml`.
pub const CONTENT_TYPE: &str = "application/vnd.yppx.marked+xml";

/// The part's description ([`TaskFlagPart`]).
pub const FLAGS: TaskFlagPart = TaskFlagPart {
    part: PART,
    content_type: CONTENT_TYPE,
    root: "marked",
    attr: "on",
    get: |t| t.marked,
    set: |t, on| t.marked = Some(on),
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Task;

    fn task(uid: i32, on: Option<bool>) -> Task {
        Task {
            uid,
            marked: on,
            ..Task::default()
        }
    }

    #[test]
    fn xml_round_trips_and_skips_unstated_tasks() {
        let tasks = vec![task(1, None), task(2, Some(true)), task(3, Some(false))];
        assert!(FLAGS.any(&tasks) && !FLAGS.any(&tasks[..1]));
        let xml = FLAGS.to_xml(&tasks);
        assert!(
            xml.contains("<marked>") && xml.contains("<task uid=\"2\" on=\"1\"/>"),
            "{xml}"
        );
        let flags = FLAGS.from_xml(&xml).unwrap();
        assert_eq!(flags, vec![(2, true), (3, false)]);
        let mut back = vec![task(1, None), task(2, None), task(3, None)];
        FLAGS.apply(&mut back, &flags);
        assert_eq!(back, tasks);
    }

    #[test]
    fn reads_later_builds_and_refuses_the_wrong_root() {
        let got = FLAGS
            .from_xml(
                "<marked v=\"2\"><task uid=\"4\" on=\"1\" x=\"y\"/>\
                 <task uid=\"x\" on=\"1\"/><task uid=\"5\" on=\"maybe\"/><other/></marked>",
            )
            .unwrap();
        assert_eq!(got, vec![(4, true)]);
        assert!(FLAGS.from_xml("<timeline/>").is_err());
        assert!(FLAGS.from_xml("<marked><task").is_err());
    }
}
