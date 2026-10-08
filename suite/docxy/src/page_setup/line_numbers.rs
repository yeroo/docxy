//! Word's Line Numbers dialog (#747): the Layout tab's Line Numbering
//! Options..., on the caret section's `w:lnNumType`. It copies the Columns
//! dialog's shape ([`super::columns_dialog`]): the caret section's values,
//! OK writes through `edit_section_setups` as one undo step, "This point
//! forward" starts the new section with a Continuous break, and "Apply to"
//! names the targets. The shown values follow the drawer's rules
//! (`crate::line_numbers` `Rule::from_setup`): Start at is one more than the
//! written `w:start`, and an absent or non-positive `w:distance` is Word's
//! Auto, a quarter inch.
use super::*;
use docxcore::sect::{LineNumbering, LnRestart};

/// "Numbering:"'s choices, in Word's order.
const RESTART_LABELS: [&str; 3] = [
    "Restart each page",
    "Restart each section",
    "Continuous",
];
const RESTARTS: [LnRestart; 3] = [
    LnRestart::NewPage,
    LnRestart::NewSection,
    LnRestart::Continuous,
];

/// Start at as the dialog shows it: one more than the written `w:start`,
/// clamped at 0 like `Rule::from_setup` (a foreign file may carry less), in
/// i64 so `w:start="2147483647"` cannot overflow.
fn start_at(ln: Option<LineNumbering>) -> i64 {
    ln.map(|l| (l.start.unwrap_or(0).max(0) as i64) + 1)
        .unwrap_or(1)
}

/// From text as the dialog shows it, in inches: a saved distance as itself
/// (0 included), Auto (no distance saved) as its quarter inch.
fn from_text(ln: Option<LineNumbering>) -> String {
    match ln.and_then(|l| l.distance) {
        Some(d) => inches(d.max(0)),
        None => inches(360),
    }
}

/// Whether From text is Auto: the section saved no distance at all. A
/// written `w:distance="0"` is From text 0, Auto off, and round-trips.
fn is_auto(ln: Option<LineNumbering>) -> bool {
    ln.and_then(|l| l.distance).is_none()
}

/// The Line Numbers dialog on the caret section's `w:lnNumType`.
pub(crate) fn line_numbers_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let (ed, _) = body_of(tab)?;
    let ln = caret_setup(ed).line_numbers;
    let restart_at = ln.map(|l| l.restart).unwrap_or_default();
    let mut d = Dialog::message(
        catalog::LINE_NUMBERS,
        "Line Numbers",
        String::new(),
        &[],
        DialogOwner::LineNumbers,
    );
    d.text = None;
    d.controls = vec![
        Control::new(
            "add",
            "Add line &numbering",
            ControlKind::Checkbox,
            Value::Bool(ln.is_some()),
        ),
        Control::new(
            "start",
            "Start &at:",
            ControlKind::Number,
            Value::Text(start_at(ln).to_string()),
        ),
        Control::new(
            "from",
            "From te&xt:",
            ControlKind::Number,
            Value::Text(from_text(ln)),
        ),
        Control::new(
            "auto",
            "&Auto",
            ControlKind::Checkbox,
            Value::Bool(is_auto(ln)),
        ),
        Control::new(
            "by",
            "Count &by:",
            ControlKind::Number,
            Value::Text(ln.map(|l| l.count_by.max(1)).unwrap_or(1).to_string()),
        ),
        choice(
            "restart",
            "Numbering:",
            ControlKind::Radio,
            &RESTART_LABELS,
            Some(
                RESTARTS
                    .iter()
                    .position(|&r| r == restart_at)
                    .unwrap_or_default(),
            ),
            None,
        ),
        apply_to(ed),
    ];
    d.buttons = ok_cancel();
    d.react = Some(Reaction(after_line_numbers_set));
    gate(&mut d);
    d.mark_opened();
    Ok(d)
}

/// The fields "Add line numbering" gates, and From text only when its Auto
/// box is off — the state Word's dialog draws. "Apply to" is never gated:
/// it also names the sections the box-off removal goes to.
fn gate(d: &mut Dialog) {
    let on = is_on(d, "add");
    for name in ["start", "auto", "by", "restart"] {
        if let Some(i) = index(d, name) {
            d.controls[i].enabled = on;
        }
    }
    if let Some(i) = index(d, "from") {
        d.controls[i].enabled = on && !is_on(d, "auto");
    }
}

/// The Reaction ([`Dialog::react`]): `gate` runs when the box or Auto
/// changes.
fn after_line_numbers_set(d: &mut Dialog, i: usize, _before: &Value) {
    if matches!(d.controls[i].name, "add" | "auto") {
        gate(d);
    }
}

/// Apply an accepted Line Numbers dialog: one undo step on the body editor,
/// whether anything changed. Like `apply_columns`, each target keeps its own
/// values for the controls the person did not change — an untouched OK
/// writes nothing, so a clean document stays clean. The box toggles only
/// where "add" changed; turning it on gives a section that had no numbering
/// the dialog's shown values (Count by and Numbering as shown, Start at
/// omitted at 1, Auto saving no `w:distance`).
pub(crate) fn apply_line_numbers(ed: &mut Editor, d: &Dialog) -> Result<bool, String> {
    let on = is_on(d, "add");
    let add_changed = d.changed("add");
    let start_changed = d.changed("start");
    let by_changed = d.changed("by");
    let from_changed = d.changed("from") || d.changed("auto");
    let restart_changed = d.changed("restart");
    // Validate the staged values before anything is written: a refused OK
    // leaves the document untouched and the dialog open. Start at shows one
    // more than `w:start`, so it may be one past i32::MAX.
    let label = |name: &str| {
        d.controls[index(d, name).unwrap_or_default()]
            .label
            .replace('&', "")
            .trim_end_matches(':')
            .to_string()
    };
    let whole = |name: &str, max: i64| -> Result<i64, String> {
        text_of(d, name)
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|v| (1..=max).contains(v))
            .ok_or_else(|| format!("{} must be a whole number of 1 or more", label(name)))
    };
    let start_at = whole("start", i64::from(i32::MAX) + 1)?;
    let count_by = i32::try_from(whole("by", i64::from(i32::MAX))?)
        .expect("validated within i32");
    // From text is a length in inches: the value is checked non-negative
    // before it is rounded to twips, so -0.0001 (which would round to 0)
    // is refused too.
    let distance = if is_on(d, "auto") {
        None
    } else {
        let inches: f64 = text_of(d, "from")
            .trim()
            .parse()
            .ok()
            .filter(|v: &f64| v.is_finite())
            .ok_or_else(|| format!("{} takes a number", label("from")))?;
        if inches < 0.0 {
            return Err(format!("{} cannot be negative", label("from")));
        }
        Some((inches * TWIPS_PER_INCH as f64).round() as i32)
    };
    let restart = RESTARTS[chosen(d, "restart").unwrap_or_default()];
    let start = i32::try_from(start_at - 1).expect("validated within i32");
    let edit = |s: &mut SectionSetup| {
        let had = s.line_numbers;
        let field_changed =
            start_changed || by_changed || from_changed || restart_changed;
        s.line_numbers = match (add_changed, on) {
            // The box stays off, or nothing but "apply" changed: leave the
            // section as it is.
            (false, false) => had,
            (false, true) if !field_changed => had,
            // The box turns off: numbering is removed.
            (true, false) => None,
            // Numbering ends on. A section that had none takes the dialog's
            // shown values; one that had numbering keeps its own for the
            // controls the person did not change.
            (_, true) => match had {
                None => Some(LineNumbering {
                    count_by: if by_changed { count_by } else { 1 },
                    start: (start_changed && start_at > 1).then_some(start),
                    distance: if from_changed { distance } else { None },
                    restart: if restart_changed {
                        restart
                    } else {
                        LnRestart::NewPage
                    },
                }),
                Some(mut ln) => {
                    if by_changed {
                        ln.count_by = count_by;
                    }
                    if start_changed {
                        ln.start = (start_at > 1).then_some(start);
                    }
                    if from_changed {
                        ln.distance = distance;
                    }
                    if restart_changed {
                        ln.restart = restart;
                    }
                    Some(ln)
                }
            },
        };
    };
    Ok(match targets(ed, d) {
        Some(k) => ed.edit_section_setups(&k, edit),
        None => {
            ed.insert_section_break_with(SectionStart::Continuous, edit)?;
            true
        }
    })
}

#[cfg(test)]
mod tests {
    use super::line_numbers_dialog;
    use crate::dialog::catalog;
    use crate::layout_tab::tests::{ed, ed_mut, setups, three_sections};
    use crate::layout_tab::{layout_checked, LayoutAct, LnChoice};
    use crate::DocTab;
    use ctlcore::json::Json;
    use docxcore::editor::Caret;
    use docxcore::sect::{LineNumbering, LnRestart, SectionStart};

    fn open(t: &mut DocTab) {
        let d = line_numbers_dialog(t).unwrap();
        t.dialogs.push(d);
    }

    fn set(t: &mut DocTab, control: &str, value: Json) {
        t.dialogs
            .set(control, &Json::obj(vec![("value", value)]))
            .unwrap_or_else(|e| panic!("{control}: {e}"));
    }

    fn s(v: &str) -> Json {
        Json::Str(v.into())
    }

    fn ok(t: &mut DocTab) -> Result<(), String> {
        crate::dialog_host::dialog_click(t, "OK")
    }

    fn shown(t: &DocTab, name: &str) -> String {
        let d = t.dialogs.top().unwrap();
        d.controls
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no control {name}"))
            .text()
    }

    fn enabled(t: &DocTab, name: &str) -> bool {
        let d = t.dialogs.top().unwrap();
        d.controls
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no control {name}"))
            .enabled
    }

    /// The dialog's controls, as (label without the accelerator, shown text).
    fn shown_labels(t: &DocTab) -> Vec<(String, String)> {
        t.dialogs
            .top()
            .unwrap()
            .controls
            .iter()
            .map(|c| (c.label.replace('&', ""), c.text()))
            .collect()
    }

    fn ln(
        count_by: i32,
        start: Option<i32>,
        distance: Option<i32>,
        restart: LnRestart,
    ) -> LineNumbering {
        LineNumbering {
            count_by,
            start,
            distance,
            restart,
        }
    }

    /// Give section `k` the line numbering `have` (`None` removes it).
    fn give(t: &mut DocTab, k: usize, have: Option<LineNumbering>) {
        ed_mut(t).edit_section_setups(&[k], |s| s.line_numbers = have);
    }

    /// Criterion 2: a section without `w:lnNumType` opens with the box off
    /// and Word's defaults, and the fields under the box are disabled.
    #[test]
    fn opens_on_a_section_without_line_numbering() {
        let mut t = three_sections();
        open(&mut t);
        let d = t.dialogs.top().unwrap();
        assert_eq!(d.title, "Line Numbers");
        assert_eq!(d.id, catalog::LINE_NUMBERS);
        assert_eq!(
            shown_labels(&t),
            [
                ("Add line numbering".to_string(), "unchecked".to_string()),
                ("Start at:".to_string(), "1".to_string()),
                ("From text:".to_string(), "0.25".to_string()),
                ("Auto".to_string(), "checked".to_string()),
                ("Count by:".to_string(), "1".to_string()),
                ("Numbering:".to_string(), "Restart each page".to_string()),
                ("Apply to:".to_string(), "This section".to_string()),
            ]
        );
        for gated in ["start", "from", "auto", "by", "restart"] {
            assert!(!enabled(&t, gated), "{gated} starts disabled");
        }
        assert!(enabled(&t, "add") && enabled(&t, "apply"));
    }

    /// Criterion 3: the saved values show, with Start at one more than the
    /// written `w:start` (Word's rule, `line_numbers.rs` `Rule::from_setup`).
    #[test]
    fn opens_on_the_sections_saved_numbering() {
        let mut t = three_sections();
        give(
            &mut t,
            1,
            Some(ln(5, Some(2), Some(360), LnRestart::NewSection)),
        );
        open(&mut t);
        assert_eq!(shown(&t, "add"), "checked");
        assert_eq!(shown(&t, "start"), "3");
        assert_eq!(shown(&t, "from"), "0.25");
        assert_eq!(shown(&t, "auto"), "unchecked");
        assert_eq!(shown(&t, "by"), "5");
        assert_eq!(shown(&t, "restart"), "Restart each section");
        for gated in ["start", "from", "auto", "by", "restart"] {
            assert!(enabled(&t, gated), "{gated} starts enabled");
        }
    }

    /// Criterion 4: OK writes all four attributes through the section-edit
    /// path, as one undo step.
    #[test]
    fn ok_writes_count_by_start_distance_restart() {
        let mut t = three_sections();
        open(&mut t);
        set(&mut t, "add", Json::Bool(true));
        set(&mut t, "start", s("3"));
        set(&mut t, "by", s("5"));
        set(&mut t, "auto", Json::Bool(false));
        set(&mut t, "from", s("0.25"));
        set(&mut t, "restart", s("Restart each section"));
        ok(&mut t).unwrap();
        assert!(t.dirty);
        let raw = &ed(&t).sections()[1];
        assert!(
            raw.contains(
                r#"<w:lnNumType w:countBy="5" w:start="2" w:distance="360" w:restart="newSection"/>"#
            ),
            "{raw}"
        );
        assert_eq!(
            setups(&t)[1].line_numbers,
            Some(ln(5, Some(2), Some(360), LnRestart::NewSection))
        );
        assert!(ed_mut(&mut t).undo(), "one undo step");
        assert_eq!(setups(&t)[1].line_numbers, None);
    }

    /// Criterion 4's omissions: Start at 1 on a section that had no `w:start`
    /// writes none, and Auto writes no `w:distance`; the section keeps its
    /// other `sectPr` children.
    #[test]
    fn start_at_one_and_auto_omit_start_and_distance() {
        let mut t = three_sections();
        open(&mut t);
        set(&mut t, "add", Json::Bool(true));
        ok(&mut t).unwrap();
        let raw = ed(&t).sections()[1].clone();
        assert!(
            raw.contains(r#"<w:lnNumType w:countBy="1" w:restart="newPage"/>"#),
            "{raw}"
        );
        assert!(raw.contains("<w:cols"), "{raw}");
        assert!(raw.contains("<w:pgMar"), "{raw}");
    }

    /// Criterion 4: OK with the box off removes `w:lnNumType` and leaves the
    /// section's other children alone.
    #[test]
    fn box_off_removes_lnnumtype() {
        let mut t = three_sections();
        give(
            &mut t,
            1,
            Some(ln(5, Some(2), Some(360), LnRestart::NewSection)),
        );
        open(&mut t);
        set(&mut t, "add", Json::Bool(false));
        ok(&mut t).unwrap();
        assert!(t.dirty);
        let raw = ed(&t).sections()[1].clone();
        assert!(!raw.contains("lnNumType"), "{raw}");
        assert!(raw.contains("<w:cols"), "{raw}");
        assert!(raw.contains("<w:pgMar"), "{raw}");
    }

    /// Criterion 5: a Start at that is not a whole number of 1 or more
    /// refuses OK with the reason, leaves the section untouched and the
    /// dialog open. (Letters never reach OK: a number field refuses them as
    /// it is typed, the catalogue's `Refuse::AtType` case.)
    #[test]
    fn invalid_start_blocks_ok_and_leaves_section_unchanged() {
        for bad in ["0", "-1", "1.5", ""] {
            let mut t = three_sections();
            give(&mut t, 1, Some(ln(5, Some(2), None, LnRestart::NewPage)));
            open(&mut t);
            set(&mut t, "add", Json::Bool(true));
            set(&mut t, "start", s(bad));
            let e = ok(&mut t).unwrap_err();
            assert!(
                e.contains("Start at") && e.contains("1 or more"),
                "{bad:?}: {e}"
            );
            assert!(t.dialogs.top().is_some(), "the dialog stays open");
            assert!(!t.dirty, "{bad:?} left the tab clean");
            assert_eq!(
                setups(&t)[1].line_numbers,
                Some(ln(5, Some(2), None, LnRestart::NewPage)),
                "{bad:?} changed the section"
            );
        }
    }

    /// Criterion 5: the same for Count by.
    #[test]
    fn invalid_count_by_blocks_ok() {
        for bad in ["0", "-2", "2.5"] {
            let mut t = three_sections();
            open(&mut t);
            set(&mut t, "add", Json::Bool(true));
            set(&mut t, "by", s(bad));
            let e = ok(&mut t).unwrap_err();
            assert!(
                e.contains("Count by") && e.contains("1 or more"),
                "{bad:?}: {e}"
            );
            assert!(t.dialogs.top().is_some(), "the dialog stays open");
            assert!(!t.dirty, "{bad:?} left the tab clean");
        }
    }

    /// Criterion 5: From text must be Auto or a non-negative length in
    /// inches. FIX r1 Immaterial: the inch value is checked before it is
    /// rounded to twips, so -0.0001 (which would round to 0) is refused too.
    #[test]
    fn invalid_from_text_blocks_ok() {
        for bad in ["-0.5", "-0.0001", ""] {
            let mut t = three_sections();
            open(&mut t);
            set(&mut t, "add", Json::Bool(true));
            set(&mut t, "auto", Json::Bool(false));
            set(&mut t, "from", s(bad));
            let e = ok(&mut t).unwrap_err();
            assert!(e.contains("From text"), "{bad:?}: {e}");
            assert!(t.dialogs.top().is_some(), "the dialog stays open");
            assert!(!t.dirty, "{bad:?} left the tab clean");
        }
    }

    /// Criterion 6: Whole document edits every section, as one undo step.
    #[test]
    fn apply_to_whole_document_edits_every_section() {
        let mut t = three_sections();
        open(&mut t);
        set(&mut t, "add", Json::Bool(true));
        set(&mut t, "by", s("2"));
        set(&mut t, "apply", s("Whole document"));
        ok(&mut t).unwrap();
        for (k, setup) in setups(&t).iter().enumerate() {
            assert_eq!(
                setup.line_numbers,
                Some(ln(2, None, None, LnRestart::NewPage)),
                "section {k}"
            );
        }
        assert!(ed_mut(&mut t).undo(), "one undo step");
        for setup in setups(&t) {
            assert_eq!(setup.line_numbers, None);
        }
    }

    /// Criterion 6: This point forward starts the new section with a
    /// Continuous break and the dialog's values; the earlier sections keep
    /// theirs.
    #[test]
    fn this_point_forward_inserts_a_continuous_break() {
        let mut t = three_sections();
        open(&mut t);
        set(&mut t, "add", Json::Bool(true));
        set(&mut t, "by", s("3"));
        set(&mut t, "apply", s("This point forward"));
        ok(&mut t).unwrap();
        let now = setups(&t);
        assert_eq!(now.len(), 4, "the break splits the caret section");
        assert_eq!(now[1].line_numbers, None, "the closed section");
        assert_eq!(
            now[2].line_numbers,
            Some(ln(3, None, None, LnRestart::NewPage)),
            "the new section from the caret forward"
        );
        assert_eq!(now[2].start, SectionStart::Continuous);
        assert_eq!(now[3].line_numbers, None, "the last section");
        assert!(ed_mut(&mut t).undo(), "one undo step");
        assert_eq!(setups(&t).len(), 3);
    }

    /// Criterion 7: Cancel drops the staged values.
    #[test]
    fn cancel_leaves_the_tab_clean() {
        let mut t = three_sections();
        open(&mut t);
        set(&mut t, "add", Json::Bool(true));
        set(&mut t, "by", s("2"));
        crate::dialog_host::dialog_click(&mut t, "Cancel").unwrap();
        assert!(t.dialogs.top().is_none());
        assert!(!t.dirty);
        assert_eq!(setups(&t)[1].line_numbers, None);
    }

    /// Criterion 7: OK with nothing changed leaves every section
    /// byte-identical and the tab clean.
    #[test]
    fn ok_with_nothing_changed_leaves_a_clean_document_clean() {
        let mut t = three_sections();
        open(&mut t);
        ok(&mut t).unwrap();
        assert!(!t.dirty);
        let mut t = three_sections();
        give(
            &mut t,
            1,
            Some(ln(5, Some(2), Some(360), LnRestart::NewSection)),
        );
        let before = ed(&t).sections();
        open(&mut t);
        ok(&mut t).unwrap();
        assert!(!t.dirty);
        assert_eq!(ed(&t).sections(), before, "byte-identical sectPrs");
    }

    /// FIX r1 Major 1: a selection running from a numbered section into an
    /// unnumbered one opens the box off; an untouched OK must leave both
    /// sections byte-identical (the box toggles only where "add" changed).
    #[test]
    fn untouched_ok_keeps_a_spanning_selections_sections() {
        let mut t = three_sections();
        give(
            &mut t,
            0,
            Some(ln(5, Some(2), Some(360), LnRestart::NewSection)),
        );
        let before = ed(&t).sections();
        ed_mut(&mut t).anchor = Some(Caret::top(0, 0));
        open(&mut t);
        assert_eq!(shown(&t, "apply"), "Selected sections");
        assert_eq!(shown(&t, "add"), "unchecked");
        ok(&mut t).unwrap();
        assert!(!t.dirty);
        assert_eq!(ed(&t).sections(), before);
    }

    /// FIX r1 Major 1: Whole document with only Count by changed keeps each
    /// section's own start, distance and restart; a section that had no
    /// numbering takes Count by with Word's defaults for the rest.
    #[test]
    fn whole_document_with_one_field_changed_keeps_each_sections_own() {
        let mut t = three_sections();
        give(
            &mut t,
            0,
            Some(ln(5, Some(2), Some(283), LnRestart::Continuous)),
        );
        give(&mut t, 1, Some(ln(3, None, None, LnRestart::NewSection)));
        open(&mut t);
        set(&mut t, "by", s("2"));
        set(&mut t, "apply", s("Whole document"));
        ok(&mut t).unwrap();
        let now = setups(&t);
        assert_eq!(
            now[0].line_numbers,
            Some(ln(2, Some(2), Some(283), LnRestart::Continuous))
        );
        assert_eq!(
            now[1].line_numbers,
            Some(ln(2, None, None, LnRestart::NewSection))
        );
        assert_eq!(
            now[2].line_numbers,
            Some(ln(2, None, None, LnRestart::NewPage))
        );
    }

    /// FIX r1 Major 1: turning the box on applies the shown values to a
    /// section that had none and leaves an already-numbered target's own
    /// values alone.
    #[test]
    fn turning_the_box_on_uses_the_shown_values_only_where_numbering_was_off() {
        let mut t = three_sections();
        give(
            &mut t,
            0,
            Some(ln(5, Some(2), Some(360), LnRestart::NewSection)),
        );
        ed_mut(&mut t).anchor = Some(Caret::top(0, 0));
        open(&mut t);
        set(&mut t, "add", Json::Bool(true));
        set(&mut t, "by", s("2"));
        ok(&mut t).unwrap();
        assert_eq!(
            setups(&t)[0].line_numbers,
            Some(ln(2, Some(2), Some(360), LnRestart::NewSection)),
            "section 0 keeps its own start, distance and restart"
        );
        let raw1 = ed(&t).sections()[1].clone();
        assert!(
            raw1.contains(r#"<w:lnNumType w:countBy="2" w:restart="newPage"/>"#),
            "{raw1}"
        );
    }

    /// FIX r1 Major 2: From text is a rounded display, so an untouched OK
    /// must not write it back — 283 would drift to 288 and 1 to 0, which the
    /// drawer reads as Auto — and a written `w:distance="0"` reopens as From
    /// text 0 with Auto off, not as Auto.
    #[test]
    fn distance_round_trips_untouched_and_zero_is_not_auto() {
        for tw in [283, 1, 0] {
            let mut t = three_sections();
            give(&mut t, 1, Some(ln(1, None, Some(tw), LnRestart::NewPage)));
            let before = ed(&t).sections();
            open(&mut t);
            ok(&mut t).unwrap();
            assert!(!t.dirty, "{tw}: an untouched OK writes nothing");
            assert_eq!(ed(&t).sections(), before, "{tw} twips drifted");
        }
        let mut t = three_sections();
        give(&mut t, 1, Some(ln(1, None, Some(0), LnRestart::NewPage)));
        open(&mut t);
        assert_eq!(shown(&t, "auto"), "unchecked");
        assert_eq!(shown(&t, "from"), "0");
        ok(&mut t).unwrap();
        assert!(!t.dirty);
    }

    /// FIX r1 Minor: `w:start="2147483647"` shows as Start at 2147483648
    /// without overflowing i32, and an untouched OK writes nothing.
    #[test]
    fn max_start_shows_without_overflow() {
        let mut t = three_sections();
        give(
            &mut t,
            1,
            Some(ln(1, Some(i32::MAX), None, LnRestart::NewPage)),
        );
        let before = ed(&t).sections();
        open(&mut t);
        assert_eq!(shown(&t, "start"), "2147483648");
        ok(&mut t).unwrap();
        assert!(!t.dirty);
        assert_eq!(ed(&t).sections(), before);
    }

    /// FIX r1 Major 1's per-field rule: a foreign negative `w:start` shows
    /// as Start at 1 (clamped like `Rule::from_setup`), an untouched OK is
    /// byte-identical, and changing another field leaves the untouched start
    /// alone. A section that already has `w:start="0"` also survives an
    /// untouched OK byte-identically.
    #[test]
    fn foreign_start_shows_clamped_and_keeps_until_changed() {
        let mut t = three_sections();
        give(&mut t, 1, Some(ln(1, Some(-3), None, LnRestart::NewPage)));
        let before = ed(&t).sections();
        open(&mut t);
        assert_eq!(shown(&t, "start"), "1");
        ok(&mut t).unwrap();
        assert!(!t.dirty, "an untouched OK writes nothing");
        assert_eq!(ed(&t).sections(), before, "the negative w:start stays");

        let mut t = three_sections();
        give(&mut t, 1, Some(ln(1, Some(-3), None, LnRestart::NewPage)));
        open(&mut t);
        set(&mut t, "by", s("2"));
        ok(&mut t).unwrap();
        let raw = ed(&t).sections()[1].clone();
        assert!(raw.contains(r#"w:start="-3""#), "{raw}");
        assert!(raw.contains(r#"w:countBy="2""#), "{raw}");

        let mut t = three_sections();
        give(&mut t, 1, Some(ln(1, Some(0), None, LnRestart::NewPage)));
        let before = ed(&t).sections();
        open(&mut t);
        assert_eq!(shown(&t, "start"), "1");
        ok(&mut t).unwrap();
        assert!(!t.dirty, "an untouched OK writes nothing");
        assert_eq!(ed(&t).sections(), before, r#"w:start="0" is preserved"#);
    }

    /// From text 0 is a valid non-negative length and is written literally
    /// (the drawer reads 0 as Auto, `Rule::from_setup`); it reopens as From
    /// text 0 with Auto off and round-trips untouched.
    #[test]
    fn zero_from_text_is_written_as_zero() {
        let mut t = three_sections();
        open(&mut t);
        set(&mut t, "add", Json::Bool(true));
        set(&mut t, "auto", Json::Bool(false));
        set(&mut t, "from", s("0"));
        ok(&mut t).unwrap();
        let raw = ed(&t).sections()[1].clone();
        assert!(
            raw.contains(r#"<w:lnNumType w:countBy="1" w:distance="0" w:restart="newPage"/>"#),
            "{raw}"
        );
        open(&mut t);
        assert_eq!(shown(&t, "auto"), "unchecked");
        assert_eq!(shown(&t, "from"), "0");
        let before = ed(&t).sections();
        ok(&mut t).unwrap();
        assert_eq!(
            ed(&t).sections(),
            before,
            "a written 0 round-trips untouched"
        );
    }

    /// Criterion 8: the Line Numbers menu checkmarks follow what the dialog
    /// saved, like the preset commands' (#746).
    #[test]
    fn the_menu_checkmarks_follow_the_dialog() {
        let mut t = three_sections();
        open(&mut t);
        set(&mut t, "add", Json::Bool(true));
        set(&mut t, "restart", s("Continuous"));
        ok(&mut t).unwrap();
        assert!(layout_checked(&t, LayoutAct::LineNumbers(LnChoice::Continuous)));
        assert!(!layout_checked(&t, LayoutAct::LineNumbers(LnChoice::RestartEachPage)));
        assert!(!layout_checked(&t, LayoutAct::LineNumbers(LnChoice::None)));
        open(&mut t);
        set(&mut t, "add", Json::Bool(false));
        ok(&mut t).unwrap();
        assert!(layout_checked(&t, LayoutAct::LineNumbers(LnChoice::None)));
    }

    /// The Reaction: turning Add line numbering on enables the fields under
    /// it — except From text, which its Auto box (on at open) keeps gating
    /// until Auto is off; the box off disables them all again. Tab order is
    /// walked by the generated inputs-typing case, not here.
    #[test]
    fn the_reaction_gates_the_fields() {
        let mut t = three_sections();
        open(&mut t);
        set(&mut t, "add", Json::Bool(true));
        for gated in ["start", "auto", "by", "restart"] {
            assert!(enabled(&t, gated), "{gated} follows the box on");
        }
        assert!(
            !enabled(&t, "from"),
            "Auto stays on, so From text stays gated"
        );
        set(&mut t, "auto", Json::Bool(false));
        assert!(enabled(&t, "from"), "a typed From text re-enables");
        set(&mut t, "auto", Json::Bool(true));
        assert!(!enabled(&t, "from"), "Auto disables From text");
        set(&mut t, "add", Json::Bool(false));
        for gated in ["start", "from", "auto", "by", "restart"] {
            assert!(!enabled(&t, gated), "{gated} follows the box off");
        }
        assert!(enabled(&t, "apply"), "Apply to is never gated");
    }

    /// The fields the catalogue lists, in the dialog's order, so the entry
    /// and `dialog-catalog-check` hold this dialog to them.
    #[test]
    fn the_dialog_matches_its_catalogue_entry() {
        let mut t = three_sections();
        open(&mut t);
        let d = t.dialogs.top().unwrap();
        let names: Vec<&str> = d.controls.iter().map(|c| c.name).collect();
        assert_eq!(names, ["add", "start", "from", "auto", "by", "restart", "apply"]);
        let json = d.catalog_check().unwrap();
        assert_eq!(json.get_str("id"), Some("line-numbers"));
    }
}
