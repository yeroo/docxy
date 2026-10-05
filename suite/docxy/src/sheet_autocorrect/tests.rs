//! AutoCorrect on `SheetView` and its dialogs (#667), without a window;
//! `uiharness/cases/sheet-autocorrect.uit` drives the same through keys and
//! the dialog verbs.

use super::*;
use crate::dialog::DialogStack;
use core::prelude::v1::test;
use ctlcore::json::Json;
use gridcore::sheet::CellValue;

fn view() -> SheetView {
    let Surface::Sheet(v) = new_sheet_surface() else {
        panic!("a new sheet surface")
    };
    v
}

fn put(v: &mut SheetView, r: u32, c: u32, text: &str) {
    let s = v.active;
    let cell = gridcore::entry::entry_cell(&mut v.pkg.workbook, s, r, c, text, None).unwrap();
    v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
}

fn select(v: &mut SheetView, r: u32, c: u32) {
    v.sel = (r, c);
    v.anchor = (r, c);
}

fn type_keys(v: &mut SheetView, text: &str) {
    for ch in text.chars() {
        v.type_char(&ch.to_string());
    }
}

/// Type `t` into (r, c) and press Enter.
fn type_enter(v: &mut SheetView, r: u32, c: u32, t: &str) {
    select(v, r, c);
    type_keys(v, t);
    assert!(v.commit_edit(), "{t}");
}

fn text(v: &SheetView, r: u32, c: u32) -> String {
    match v.sheet().cell(r, c).map(|c| c.value.clone()) {
        Some(CellValue::Text(t)) => t,
        other => panic!("{other:?}"),
    }
}

#[test]
fn ent_case_040_corrects_as_typed_and_never_a_formula() {
    let mut v = view();
    type_enter(&mut v, 0, 0, "(c) 2024");
    type_enter(&mut v, 1, 0, "teh cat");
    type_enter(&mut v, 2, 0, "=\"(c)\"");
    type_enter(&mut v, 3, 0, "monday meeting");
    type_enter(&mut v, 4, 0, "THursday");
    type_enter(&mut v, 5, 0, "teh");
    assert_eq!(text(&v, 0, 0), "\u{a9} 2024");
    assert_eq!(text(&v, 1, 0), "the cat");
    assert_eq!(text(&v, 2, 0), "(c)");
    assert_eq!(
        v.sheet().cell(2, 0).unwrap().formula.as_deref(),
        Some("\"(c)\"")
    );
    assert_eq!(text(&v, 3, 0), "Monday meeting");
    assert_eq!(text(&v, 4, 0), "Thursday");
    assert_eq!(text(&v, 5, 0), "the", "at the commit");
    // A7: `teh ` corrects, proposing A2's `the cat`; Ctrl+Z takes back the
    // correction and the proposal it made; `x` and Enter.
    select(&mut v, 6, 0);
    type_keys(&mut v, "teh ");
    assert_eq!(v.editing.as_deref(), Some("the cat"));
    assert!(v.edit_proposal.is_some());
    // `sheet_key` runs this before Ctrl+Z reaches the editor.
    v.proposal_before_key("z", true, false);
    v.edit_revert();
    assert_eq!(v.editing.as_deref(), Some("teh "));
    assert_eq!(v.edit_caret, 4);
    type_keys(&mut v, "x");
    assert!(v.commit_edit());
    assert_eq!(text(&v, 6, 0), "teh x");
}

#[test]
fn proposals_numbers_and_untyped_words_are_left_alone() {
    let mut v = view();
    put(&mut v, 0, 0, "Teh Corp");
    type_enter(&mut v, 1, 0, "teh");
    assert_eq!(text(&v, 1, 0), "Teh Corp", "a taken proposal");
    type_enter(&mut v, 0, 3, "1/2");
    assert!(matches!(
        v.sheet().cell(0, 3).unwrap().value,
        CellValue::Number(_)
    ));
    // FIX r2 m3: an F2 fix elsewhere leaves a last word nobody typed.
    put(&mut v, 1, 3, "Meting on monday");
    select(&mut v, 1, 3);
    v.begin_cell_edit(None);
    v.edit_caret = 2;
    v.type_char("e");
    assert!(v.commit_edit());
    assert_eq!(text(&v, 1, 3), "Meeting on monday");
    // A caret move after the typing ends it too.
    select(&mut v, 2, 3);
    type_keys(&mut v, "x teh");
    v.edit_move(-1);
    v.edit_move(1);
    assert!(v.commit_edit());
    assert_eq!(text(&v, 2, 3), "x teh");
    // Ctrl+Z with no correction pending reverts the typing.
    select(&mut v, 3, 3);
    type_keys(&mut v, "abc");
    v.edit_revert();
    assert_eq!(v.editing.as_deref(), Some(""));
    v.end_cell_edit();
    // A pick from the list is the column's spelling.
    put(&mut v, 4, 4, "teh");
    select(&mut v, 5, 4);
    assert!(v.pick_value("teh"));
    assert_eq!(text(&v, 5, 4), "teh");
}

#[test]
fn ent_case_056_typed_urls_are_hyperlinks_in_one_undo_step() {
    let mut v = view();
    type_enter(&mut v, 2, 0, "https://example.com");
    assert_eq!(
        v.sheet().hyperlinks.get(&(2, 0)).map(String::as_str),
        Some("https://example.com")
    );
    assert!(v.undo_step());
    assert!(v.sheet().cell(2, 0).is_none_or(|c| c.value.is_empty()));
    assert!(!v.sheet().hyperlinks.contains_key(&(2, 0)));
    let mut ac = (*v.autocorrect).clone();
    ac.opts.hyperlinks = false;
    v.autocorrect = Rc::new(ac);
    type_enter(&mut v, 3, 0, "https://example.org");
    assert!(!v.sheet().hyperlinks.contains_key(&(3, 0)));
    assert_eq!(text(&v, 3, 0), "https://example.org");
}

fn set(stack: &mut DialogStack, name: &str, value: &str) {
    stack
        .set(name, &Json::obj(vec![("value", Json::Str(value.into()))]))
        .unwrap();
}

fn press(stack: &mut DialogStack, ac: &mut AutoCorrect, button: &str) -> bool {
    click(stack, ac, button).expect("ours").unwrap()
}

#[test]
fn ent_case_056_the_dialog_adds_redefines_deletes_and_switches() {
    let mut ac = AutoCorrect::default();
    let mut stack = DialogStack::default();
    stack.push(dialog(&ac));
    let d = stack.top().unwrap();
    assert_eq!(
        d.tabs,
        [
            "AutoCorrect",
            "AutoFormat As You Type",
            "Actions",
            "Math AutoCorrect"
        ]
    );
    set(&mut stack, "replace", "cdp");
    set(&mut stack, "with", "Consolidated Data Processing");
    assert!(press(&mut stack, &mut ac, "Add"));
    assert_eq!(ac.lookup("cdp"), Some("Consolidated Data Processing"));
    // Re-adding with another With asks; No keeps it, Yes replaces it.
    set(&mut stack, "replace", "cdp");
    set(&mut stack, "with", "Other");
    assert!(!press(&mut stack, &mut ac, "Add"));
    assert_eq!(stack.top().unwrap().id, "autocorrect-redefine");
    assert!(!press(&mut stack, &mut ac, "No"));
    assert_eq!(ac.lookup("cdp"), Some("Consolidated Data Processing"));
    assert!(!press(&mut stack, &mut ac, "Add"));
    assert!(press(&mut stack, &mut ac, "Yes"));
    assert_eq!(ac.lookup("cdp"), Some("Other"));
    assert_eq!(stack.top().unwrap().id, "autocorrect");
    // Choosing an entry fills the fields; Delete removes it.
    let d = stack.top().unwrap();
    let items = &d
        .controls
        .iter()
        .find(|c| c.name == "entries")
        .unwrap()
        .items;
    let at = items.iter().position(|i| i.starts_with("cdp ")).unwrap();
    stack
        .set(
            "entries",
            &Json::obj(vec![("value", Json::Str(items[at].clone()))]),
        )
        .unwrap();
    assert_eq!(text_of(stack.top().unwrap(), "with"), "Other");
    assert!(press(&mut stack, &mut ac, "Delete"));
    assert_eq!(ac.lookup("cdp"), None);
    // Exceptions: INitial CAps `ABc`.
    assert!(!press(&mut stack, &mut ac, "Exceptions..."));
    assert_eq!(stack.top().unwrap().tabs, ["First Letter", "INitial CAps"]);
    stack.select_tab("INitial CAps").unwrap();
    set(&mut stack, "caps-word", "ABc");
    assert!(press(&mut stack, &mut ac, "Add"));
    assert_eq!(ac.exceptions(ExceptionKind::InitialCaps), ["ABc"]);
    press(&mut stack, &mut ac, "OK");
    assert_eq!(stack.top().unwrap().id, "autocorrect");
    // OK takes the switches: hyperlinks off.
    stack.select_tab("AutoFormat As You Type").unwrap();
    stack
        .set(
            "ac_hyperlinks",
            &Json::obj(vec![("value", Json::Bool(false))]),
        )
        .unwrap();
    assert!(press(&mut stack, &mut ac, "OK"));
    assert!(!ac.opts.hyperlinks);
    assert!(!stack.is_open());
}

fn text_of(d: &Dialog, name: &str) -> String {
    super::text(d, name)
}

#[test]
fn cancel_keeps_the_switches_and_another_dialog_is_not_ours() {
    let mut ac = AutoCorrect::default();
    let mut stack = DialogStack::default();
    stack.push(dialog(&ac));
    stack
        .set(
            "ac_replace_text",
            &Json::obj(vec![("value", Json::Bool(false))]),
        )
        .unwrap();
    assert!(!press(&mut stack, &mut ac, "Cancel"));
    assert!(ac.opts.replace_text);
    stack.push(Dialog::message(
        "t",
        "T",
        String::new(),
        &[("OK", ButtonRole::Accept)],
        DialogOwner::Test,
    ));
    assert!(click(&mut stack, &mut ac, "OK").is_none());
}
