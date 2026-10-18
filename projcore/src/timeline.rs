//! Which tasks show on the Timeline (Task Information's Display on
//! Timeline, #1536). MSPDI has no element for it, so it is saved only in the
//! `.yppx` package's `timeline.xml` part ([`crate::yppx`]), one `<task>` per
//! task that states a value; the task's [`crate::model::Task::display_on_timeline`]
//! holds it in the model.

use crate::formatting::{decode, push_attr};
use crate::model::Task;
use opccore::xml::{Event, XmlParser};

/// The `.yppx` part the flags are saved in.
pub const PART: &str = "timeline.xml";
/// Its content type in `[Content_Types].xml`.
pub const CONTENT_TYPE: &str = "application/vnd.yppx.timeline+xml";

/// Whether any task states a value, so the part is written at all.
pub fn any(tasks: &[Task]) -> bool {
    tasks.iter().any(|t| t.display_on_timeline.is_some())
}

/// The `timeline.xml` part.
pub fn to_xml(tasks: &[Task]) -> String {
    let mut out =
        String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<timeline>\n");
    for t in tasks {
        if let Some(on) = t.display_on_timeline {
            out.push_str("  <task");
            push_attr(&mut out, "uid", &t.uid.to_string());
            push_attr(&mut out, "show", if on { "1" } else { "0" });
            out.push_str("/>\n");
        }
    }
    out.push_str("</timeline>\n");
    out
}

/// Read a `timeline.xml` part into (uid, shown) pairs. Unknown elements are
/// ignored and a `<task>` without a readable uid or show is skipped.
pub fn from_xml(xml: &str) -> Result<Vec<(i32, bool)>, String> {
    let mut parser = XmlParser::new(xml);
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut seen_root = false;
    loop {
        match parser.next() {
            Event::Start => {
                let name = parser.name();
                if depth == 0 {
                    if name != "timeline" {
                        return Err(format!("{PART}: the root is <{name}>, not <timeline>"));
                    }
                    seen_root = true;
                } else if depth == 1 && name == "task" {
                    let uid = decode(parser.attr("uid")).trim().parse::<i32>().ok();
                    let show = match decode(parser.attr("show")).trim() {
                        "1" => Some(true),
                        "0" => Some(false),
                        _ => None,
                    };
                    if let (Some(uid), Some(show)) = (uid, show) {
                        out.push((uid, show));
                    }
                }
                depth += 1;
            }
            Event::End => depth = depth.saturating_sub(1),
            Event::Text => {}
            Event::Eof => break,
        }
    }
    if parser.is_malformed() || !seen_root {
        return Err(format!("{PART}: not well-formed XML"));
    }
    Ok(out)
}

/// Put read flags on their tasks; a uid no task has is dropped.
pub fn apply(tasks: &mut [Task], flags: &[(i32, bool)]) {
    for &(uid, on) in flags {
        if let Some(t) = tasks.iter_mut().find(|t| t.uid == uid) {
            t.display_on_timeline = Some(on);
        }
    }
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
