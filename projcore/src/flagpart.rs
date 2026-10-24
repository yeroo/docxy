//! A `.yppx` part holding one per-task flag MSPDI has no element for: one
//! `<task uid=".." attr="1|0"/>` per task that states a value. Display on
//! Timeline ([`crate::timeline`], #1536) and Marked ([`crate::marked`],
//! #1667) are saved this way.

use crate::formatting::{decode, push_attr};
use crate::model::Task;
use opccore::xml::{Event, XmlParser};

/// One flag's part: its name, content type, root element, the `<task>`
/// attribute holding the value, and the task field it is kept in.
pub struct TaskFlagPart {
    pub part: &'static str,
    pub content_type: &'static str,
    pub root: &'static str,
    pub attr: &'static str,
    pub get: fn(&Task) -> Option<bool>,
    pub set: fn(&mut Task, bool),
}

impl TaskFlagPart {
    /// Whether any task states a value, so the part is written at all.
    pub fn any(&self, tasks: &[Task]) -> bool {
        tasks.iter().any(|t| (self.get)(t).is_some())
    }

    /// The part's XML.
    pub fn to_xml(&self, tasks: &[Task]) -> String {
        let mut out = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<{}>\n",
            self.root
        );
        for t in tasks {
            if let Some(on) = (self.get)(t) {
                out.push_str("  <task");
                push_attr(&mut out, "uid", &t.uid.to_string());
                push_attr(&mut out, self.attr, if on { "1" } else { "0" });
                out.push_str("/>\n");
            }
        }
        out.push_str(&format!("</{}>\n", self.root));
        out
    }

    /// Read the part into (uid, value) pairs. Unknown elements are ignored
    /// and a `<task>` without a readable uid or value is skipped.
    pub fn from_xml(&self, xml: &str) -> Result<Vec<(i32, bool)>, String> {
        let mut parser = XmlParser::new(xml);
        let mut out = Vec::new();
        let mut depth = 0usize;
        let mut seen_root = false;
        loop {
            match parser.next() {
                Event::Start => {
                    let name = parser.name();
                    if depth == 0 {
                        if name != self.root {
                            return Err(format!(
                                "{}: the root is <{name}>, not <{}>",
                                self.part, self.root
                            ));
                        }
                        seen_root = true;
                    } else if depth == 1 && name == "task" {
                        let uid = decode(parser.attr("uid")).trim().parse::<i32>().ok();
                        let on = match decode(parser.attr(self.attr)).trim() {
                            "1" => Some(true),
                            "0" => Some(false),
                            _ => None,
                        };
                        if let (Some(uid), Some(on)) = (uid, on) {
                            out.push((uid, on));
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
            return Err(format!("{}: not well-formed XML", self.part));
        }
        Ok(out)
    }

    /// Put read values on their tasks; a uid no task has is dropped.
    pub fn apply(&self, tasks: &mut [Task], flags: &[(i32, bool)]) {
        for &(uid, on) in flags {
            if let Some(t) = tasks.iter_mut().find(|t| t.uid == uid) {
                (self.set)(t, on);
            }
        }
    }
}
