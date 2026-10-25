//! Rich task notes in the formatting part (#1801). `Task::rich_notes` is
//! not formatting, but it travels where the formatting does: the `.yppx`
//! `formatting.xml` part and the text a format plugin's `_ex` symbols carry
//! (#1702), so a `.mpp` plugin gets the markup on write and returns it on
//! read without a new ABI symbol.
//!
//! Shape: one `<note uid="N" markup="…"/>` per task with rich notes, in task
//! order, after the formatting's own children. `markup` is the canonical
//! [`crate::richtext`] markup with the attribute escapes plus `&#10;` for a
//! newline and `&#9;` for a tab (an XML reader turns a raw one into a space).
//! The other C0 controls cannot be written in XML 1.0 at all; they are
//! written as they are and read back by this reader only.
//!
//! On read a `<note>` is applied only to an existing, non-blank task whose
//! plain notes are the markup's plain text, so old formatting never lands on
//! other text; any other is dropped. [`Formatting::from_xml`] ignores
//! `<note>`, as an older build does, which then saves the part without them.
//!
//! [`Formatting::from_xml`]: super::Formatting::from_xml

use super::{PART, decode};
use crate::json::Json;
use crate::model::Project;
use opccore::xml::{Event, XmlParser};

/// The `formatting.xml` text for `proj`: its formatting and the tasks' rich
/// notes, or `None` when there is neither (no part is written, and a plugin
/// gets no formatting text). Without rich notes it is exactly
/// [`super::Formatting::to_xml`].
pub fn part_xml(proj: &Project) -> Option<String> {
    let noted = || proj.tasks.iter().filter(|t| t.rich_notes.is_some());
    if proj.formatting.is_empty() && noted().next().is_none() {
        return None;
    }
    let mut xml = proj.formatting.to_xml();
    let mut notes = String::new();
    for t in noted() {
        notes.push_str("  <note");
        super::push_attr(&mut notes, "uid", &t.uid.to_string());
        push_markup(&mut notes, t.rich_notes.as_deref().unwrap_or(""));
        notes.push_str("/>\n");
    }
    if !notes.is_empty() {
        let end = xml.rfind("</formatting>").unwrap_or(xml.len());
        xml.insert_str(end, &notes);
    }
    Some(xml)
}

/// ` markup="…"`, escaping what an attribute must keep: the markup's tags,
/// quotes, and its newlines and tabs as character references.
fn push_markup(out: &mut String, value: &str) {
    out.push_str(" markup=\"");
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\t' => out.push_str("&#9;"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The `<note>`s of a `formatting.xml` text as `(uid, markup)`, in order.
/// A note without a numeric `uid` is skipped.
pub fn read_notes(xml: &str) -> Result<Vec<(i32, String)>, String> {
    let mut parser = XmlParser::new(xml);
    let mut out = Vec::new();
    let mut depth = 0usize;
    loop {
        match parser.next() {
            Event::Start => {
                if depth == 1 && parser.name() == "note" {
                    let uid = decode(parser.attr("uid")).trim().parse::<i32>().ok();
                    if let Some(uid) = uid {
                        out.push((uid, decode(parser.attr("markup"))));
                    }
                }
                depth += 1;
            }
            Event::End => depth = depth.saturating_sub(1),
            Event::Text => {}
            Event::Eof => break,
        }
    }
    if parser.is_malformed() {
        return Err(format!("{PART}: not well-formed XML"));
    }
    Ok(out)
}

/// Give `proj`'s tasks the rich notes of a `formatting.xml` text, read after
/// the tasks themselves (MSPDI). A note whose task is missing or blank, whose
/// plain text is not the task's notes, or that formats nothing is dropped.
pub fn apply_notes(proj: &mut Project, xml: &str) -> Result<(), String> {
    for (uid, markup) in read_notes(xml)? {
        let Some(task) = proj.tasks.iter_mut().find(|t| t.uid == uid && !t.is_null) else {
            continue;
        };
        let (plain, rich) = crate::richtext::split_notes(&markup);
        let notes = task.notes.as_deref().map(crate::text::normalize_newlines);
        if rich.is_some() && plain == notes {
            task.rich_notes = rich;
        }
    }
    Ok(())
}

/// The UI plugin requests' read-only `view.rich_notes`: `{"<uid>": markup}`
/// for every task with rich notes. A task not in it has plain notes only
/// (MSPDI `<Notes>`, to be read as [`crate::richtext::Rich::plain`]).
pub fn rich_notes_json(proj: &Project) -> Json {
    Json::Obj(
        proj.tasks
            .iter()
            .filter_map(|t| Some((t.uid.to_string(), Json::str(t.rich_notes.clone()?))))
            .collect(),
    )
}

#[cfg(test)]
mod tests;
