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
/// clamped at 0 like `Rule::from_setup` (a foreign file may carry less).
fn start_at(ln: Option<LineNumbering>) -> i32 {
    ln.map(|l| l.start.unwrap_or(0).max(0) + 1).unwrap_or(1)
}

/// From text in twips as the dialog shows it: absent or non-positive is
/// Auto's quarter inch.
fn distance_tw(ln: Option<LineNumbering>) -> i32 {
    ln.and_then(|l| l.distance)
        .filter(|&d| d > 0)
        .unwrap_or(360)
}

/// Whether From text is Auto: the section saved no positive distance.
fn is_auto(ln: Option<LineNumbering>) -> bool {
    ln.map(|l| l.distance.unwrap_or(0) <= 0).unwrap_or(true)
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
            Value::Text(inches(distance_tw(ln))),
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
/// whether anything changed. Every section the dialog applies to gets what
/// the dialog shows; `w:start` is omitted only when Start at is 1 and the
/// section had none, so an untouched section keeps its own shape (a saved
/// `w:start="0"` stays).
pub(crate) fn apply_line_numbers(ed: &mut Editor, d: &Dialog) -> Result<bool, String> {
    let on = is_on(d, "add");
    // Validate everything before anything is written: a refused OK leaves
    // the document untouched and the dialog open.
    let (count_by, start_at, distance, restart) = if on {
        let label = |name: &str| {
            d.controls[index(d, name).unwrap_or_default()]
                .label
                .replace('&', "")
                .trim_end_matches(':')
                .to_string()
        };
        let whole = |name: &str| -> Result<i32, String> {
            text_of(d, name)
                .trim()
                .parse::<i32>()
                .ok()
                .filter(|v| *v >= 1)
                .ok_or_else(|| format!("{} must be a whole number of 1 or more", label(name)))
        };
        let start_at = whole("start")?;
        let count_by = whole("by")?;
        let distance = if is_on(d, "auto") {
            None
        } else {
            let tw =
                twips_of(&text_of(d, "from")).ok_or_else(|| format!("{} takes a number", label("from")))?;
            if tw < 0 {
                return Err(format!("{} cannot be negative", label("from")));
            }
            Some(tw)
        };
        (count_by, start_at, distance, RESTARTS[chosen(d, "restart").unwrap_or_default()])
    } else {
        (1, 1, None, LnRestart::NewPage)
    };
    let edit = |s: &mut SectionSetup| {
        s.line_numbers = if !on {
            None
        } else {
            let keep_none = start_at == 1 && s.line_numbers.and_then(|l| l.start).is_none();
            Some(LineNumbering {
                count_by,
                start: if keep_none { None } else { Some(start_at - 1) },
                distance,
                restart,
            })
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
    /// inches.
    #[test]
    fn invalid_from_text_blocks_ok() {
        for bad in ["-0.5", ""] {
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

    /// The edge pins: a foreign negative `w:start` shows as Start at 1
    /// (clamped like `Rule::from_setup`) and OK writes back the sane 0; a
    /// section that already has `w:start="0"` keeps it on an untouched OK.
    #[test]
    fn negative_start_clamps_and_zero_start_survives() {
        let mut t = three_sections();
        give(&mut t, 1, Some(ln(1, Some(-3), None, LnRestart::NewPage)));
        open(&mut t);
        assert_eq!(shown(&t, "start"), "1");
        ok(&mut t).unwrap();
        let raw = ed(&t).sections()[1].clone();
        assert!(
            raw.contains(r#"<w:lnNumType w:countBy="1" w:start="0" w:restart="newPage"/>"#),
            "{raw}"
        );

        let mut t = three_sections();
        give(&mut t, 1, Some(ln(1, Some(0), None, LnRestart::NewPage)));
        let before = ed(&t).sections();
        open(&mut t);
        assert_eq!(shown(&t, "start"), "1");
        ok(&mut t).unwrap();
        assert!(!t.dirty, "an untouched OK writes nothing");
        assert_eq!(ed(&t).sections(), before, r#"w:start="0" is preserved"#);
    }

    /// The edge pin: From text 0 is a valid non-negative length and is
    /// written literally (the drawer reads 0 as Auto, `Rule::from_setup`).
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
