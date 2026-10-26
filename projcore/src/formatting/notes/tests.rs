use super::*;
use crate::formatting::Formatting;
use crate::model::Task;

fn task(uid: i32, notes: Option<&str>, rich: Option<&str>) -> Task {
    Task {
        uid,
        id: uid,
        name: format!("T{uid}"),
        notes: notes.map(str::to_string),
        rich_notes: rich.map(str::to_string),
        ..Task::default()
    }
}

/// Every awkward thing a note can hold: bold, italic, a bullet, a centred
/// line, escapes, a tab, spaces at both ends, an empty middle line.
const RICH: &str = "<p align=\"center\"><b>Title</b></p>\n<li><i>a &amp; &lt;b&gt;</i>\t\"q\"</li>\n<p></p>\n<p> end </p>";
const PLAIN: &str = "Title\na & <b>\t\"q\"\n\n end ";

fn plan() -> Project {
    Project {
        tasks: vec![
            task(1, Some(PLAIN), Some(RICH)),
            task(2, Some("plain only"), None),
            task(3, Some("x"), Some("<li>x</li>")),
        ],
        ..Project::default()
    }
}

/// The tasks as they come out of MSPDI: plain notes, no markup.
fn stripped(p: &Project) -> Project {
    let mut p = p.clone();
    for t in &mut p.tasks {
        t.rich_notes = None;
    }
    p
}

#[test]
fn notes_round_trip_in_formatting_xml() {
    let proj = plan();
    let xml = part_xml(&proj).expect("rich notes alone make a part");
    // Newlines and tabs are character references; no raw one is left in the
    // attribute, so a conforming XML reader keeps them.
    let line = xml.lines().find(|l| l.contains("uid=\"1\"")).unwrap();
    assert!(line.contains("&#10;") && line.contains("&#9;"), "{line}");
    assert!(!line.contains('\t'));
    assert!(xml.contains("  <note uid=\"3\" markup=\"&lt;li&gt;x&lt;/li&gt;\"/>\n</formatting>\n"));
    assert!(
        !xml.contains("uid=\"2\""),
        "a plain-only task writes no note"
    );
    assert_eq!(
        read_notes(&xml).unwrap(),
        vec![(1, RICH.to_string()), (3, "<li>x</li>".to_string())]
    );
    let mut back = stripped(&proj);
    apply_notes(&mut back, &xml).unwrap();
    assert_eq!(back, proj);
}

#[test]
fn no_rich_notes_formatting_xml_is_byte_identical() {
    let mut proj = stripped(&plan());
    assert_eq!(part_xml(&proj), None, "nothing to write, no part");
    proj.formatting.palette = Some("fx.blue".into());
    proj.formatting.set_switch("flag", Some(true));
    assert_eq!(part_xml(&proj), Some(proj.formatting.to_xml()));
}

#[test]
fn notes_follow_the_formatting() {
    let mut proj = plan();
    proj.formatting.palette = Some("fx.blue".into());
    let xml = part_xml(&proj).unwrap();
    // The formatting reads back as it was; its reader ignores the notes, as
    // an older build does.
    assert_eq!(Formatting::from_xml(&xml).unwrap(), proj.formatting);
    let mut back = stripped(&proj);
    apply_notes(&mut back, &xml).unwrap();
    assert_eq!(back.tasks, proj.tasks);
}

#[test]
fn from_xml_ignores_note_elements() {
    let mut f = Formatting::default();
    f.link_style = Some("curved".into());
    let without = f.to_xml();
    let with = without.replace(
        "</formatting>",
        "  <note uid=\"1\" markup=\"&lt;li&gt;x&lt;/li&gt;\"/>\n</formatting>",
    );
    assert_eq!(
        Formatting::from_xml(&with).unwrap(),
        Formatting::from_xml(&without).unwrap()
    );
}

#[test]
fn stale_note_is_dropped_on_apply() {
    let xml = part_xml(&plan()).unwrap();
    let mut proj = stripped(&plan());
    // Task 1's text changed since the note was written; task 3 is now a
    // blank row; a note for uid 9 has no task.
    proj.tasks[0].notes = Some("Title\nsomething else".into());
    proj.tasks[2].is_null = true;
    let xml = xml.replace(
        "</formatting>",
        "  <note uid=\"9\" markup=\"&lt;li&gt;x&lt;/li&gt;\"/>\n</formatting>",
    );
    let before = proj.clone();
    apply_notes(&mut proj, &xml).unwrap();
    assert_eq!(proj, before);
    // A note that formats nothing is no rich note, and a stored `\r\n` in the
    // task's notes still matches.
    let mut proj = Project {
        tasks: vec![task(1, Some("a\r\nb"), None), task(2, Some("x"), None)],
        ..Project::default()
    };
    let xml = "<formatting>\n  <note uid=\"1\" markup=\"&lt;li&gt;a&lt;/li&gt;&#10;&lt;p&gt;b&lt;/p&gt;\"/>\n  <note uid=\"2\" markup=\"&lt;p&gt;x&lt;/p&gt;\"/>\n  <note markup=\"&lt;li&gt;x&lt;/li&gt;\"/>\n</formatting>\n";
    apply_notes(&mut proj, xml).unwrap();
    assert_eq!(
        proj.tasks[0].rich_notes.as_deref(),
        Some("<li>a</li>\n<p>b</p>")
    );
    assert_eq!(proj.tasks[1].rich_notes, None);
}

#[test]
fn rich_notes_json_lists_the_formatted_tasks_only() {
    assert_eq!(
        rich_notes_json(&plan()),
        Json::obj(vec![("1", Json::str(RICH)), ("3", Json::str("<li>x</li>"))])
    );
    assert_eq!(rich_notes_json(&stripped(&plan())), Json::Obj(Vec::new()));
}
