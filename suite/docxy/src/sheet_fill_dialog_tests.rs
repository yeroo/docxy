//! The fill gestures and dialogs of `sheet_fill` (#668), without a window.

use super::*;
use crate::{Kind, new_sheet_surface};
use gridcore::sheet::{Cell, CellValue, parse_cell_name};

fn tab() -> DocTab {
    DocTab {
        kind: Kind::Xlsx,
        title: "book.xlsx".into(),
        path: None,
        surface: new_sheet_surface(),
        dirty: false,
        status: "".into(),
        comments: vec![],
        tracked_comment_ids: Default::default(),
        comments_removed_all: false,
        used_comment_ids: Default::default(),
        pkg: None,
        notes: vec![],
        markdown: false,
        hf_edit: None,
        bundle_html: None,
        load_failed: false,
        dialogs: crate::dialog::DialogStack::default(),
        access: crate::open_mode::Access::default(),
        last_hot: Default::default(),
        converted_docx: None,
        pending_conversion: false,
        mail: Default::default(),
        import: Default::default(),
    }
}

fn view(t: &mut DocTab) -> &mut SheetView {
    match &mut t.surface {
        Surface::Sheet(v) => v,
        _ => panic!("a sheet"),
    }
}

fn at(name: &str) -> (u32, u32) {
    parse_cell_name(name).unwrap()
}

fn rect(s: &str) -> (u32, u32, u32, u32) {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let (r0, c0) = at(a);
    let (r1, c1) = at(b);
    (r0, c0, r1, c1)
}

fn put(v: &mut SheetView, name: &str, cell: Cell) {
    let (r, c) = at(name);
    let s = v.active;
    v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
}

fn shown(v: &SheetView, name: &str) -> CellValue {
    let (r, c) = at(name);
    v.sheet()
        .cell(r, c)
        .map(|c| c.value.clone())
        .unwrap_or_default()
}

fn req<'a>(src: &str, to: &str, kind: FillKind, lists: &'a [Vec<String>]) -> FillReq<'a> {
    FillReq {
        src: rect(src),
        to: at(to),
        kind,
        ctrl: false,
        lists,
    }
}

fn set(t: &mut DocTab, name: &str, value: Value) {
    let d = t.dialogs.top_dialog_mut().expect("a dialog");
    d.controls
        .iter_mut()
        .find(|c| c.name == name)
        .unwrap()
        .value = value;
}

#[test]
fn a_fill_is_one_undo_step_and_leaves_its_box_selected() {
    let mut t = tab();
    let v = view(&mut t);
    put(v, "A1", Cell::text("Item 1"));
    let filled = v.fill_drag(&req("A1", "A4", FillKind::Auto, &[])).unwrap();
    assert_eq!(filled, Some(Filled::Extended(rect("A2:A4"))));
    assert_eq!(shown(v, "A4"), CellValue::Text("Item 4".into()));
    assert_eq!(v.range(), rect("A1:A4"));
    assert_eq!(v.undo.len(), 1);
    // Up: backwards.
    put(v, "C5", Cell::number(3.0));
    put(v, "C6", Cell::number(4.0));
    v.fill_drag(&req("C5:C6", "C3", FillKind::Auto, &[]))
        .unwrap();
    assert_eq!(shown(v, "C3"), CellValue::Number(1.0));
    assert_eq!(v.range(), rect("C3:C6"));
    // Back inside: the cells left behind clear, and the selection shrinks.
    let cleared = v
        .fill_drag(&req("A1:A4", "A2", FillKind::Auto, &[]))
        .unwrap();
    assert_eq!(cleared, Some(Filled::Cleared(rect("A3:A4"))));
    assert_eq!(shown(v, "A3"), CellValue::Empty);
    assert_eq!(v.range(), rect("A1:A2"));
    // Back onto the handle: nothing, and no step.
    let steps = v.undo.len();
    assert_eq!(
        v.fill_drag(&req("A1:A2", "A2", FillKind::Auto, &[])),
        Ok(None)
    );
    assert_eq!(v.undo.len(), steps);
}

#[test]
fn auto_fill_options_redo_the_fill_as_one_step() {
    let mut t = tab();
    let v = view(&mut t);
    put(v, "A1", Cell::number(1.0));
    put(v, "A2", Cell::number(2.0));
    v.fill_drag(&req("A1:A2", "A4", FillKind::Auto, &[]))
        .unwrap();
    assert_eq!(shown(v, "A4"), CellValue::Number(4.0));
    let opts = FillOptions {
        view: v.id,
        edit_gen: v.edit_gen,
        src: rect("A1:A2"),
        to: at("A4"),
        kind: FillKind::Auto,
        ctrl: false,
        dest: rect("A3:A4"),
        dates: false,
        numbers: true,
    };
    assert!(opts.kinds().contains(&FillKind::GrowthTrend));
    v.refill(&opts, FillKind::Copy, &[]).unwrap();
    assert_eq!(shown(v, "A3"), CellValue::Number(1.0));
    assert_eq!(shown(v, "A4"), CellValue::Number(2.0));
    assert_eq!(v.undo.len(), 1, "the copy replaced the series' step");
    assert!(v.undo_step());
    assert_eq!(shown(v, "A3"), CellValue::Empty, "one undo: before either");
    // After another edit the button is gone.
    let mut t = tab();
    let v = view(&mut t);
    put(v, "A1", Cell::number(1.0));
    v.fill_drag(&req("A1", "A2", FillKind::Auto, &[])).unwrap();
    let stale = FillOptions {
        view: v.id,
        edit_gen: v.edit_gen,
        src: rect("A1"),
        to: at("A2"),
        kind: FillKind::Auto,
        ctrl: false,
        dest: rect("A2"),
        dates: false,
        numbers: true,
    };
    v.push_undo();
    assert!(!stale.stands(v));
    assert!(v.refill(&stale, FillKind::Series, &[]).is_err());
}

#[test]
fn the_series_dialog_stages_its_spec_and_fills() {
    let mut t = tab();
    let v = view(&mut t);
    put(v, "A1", Cell::number(1.0));
    v.anchor = at("A1");
    v.sel = at("A5");
    t.dialogs.push(series_dialog(&t).unwrap());
    let d = t.dialogs.top().unwrap();
    assert_eq!(choice(d, "series-in"), Some(1), "a column: Columns");
    set(&mut t, "step", Value::Text("2".into()));
    set(&mut t, "stop", Value::Text("6".into()));
    let spec = staged_series(t.dialogs.top().unwrap()).unwrap();
    assert_eq!(
        (spec.rows, spec.step, spec.stop, spec.kind),
        (false, 2.0, Some(6.0), SeriesType::Linear)
    );
    let v = view(&mut t);
    assert_eq!(v.fill_series(&spec, &[]), Ok(2));
    assert_eq!(shown(v, "A3"), CellValue::Number(5.0));
    assert_eq!(shown(v, "A4"), CellValue::Empty, "7 is past the stop");
    assert_eq!(v.undo.len(), 1);
    set(&mut t, "step", Value::Text("two".into()));
    assert!(staged_series(t.dialogs.top().unwrap()).is_err());
}

#[test]
fn justify_rewraps_and_asks_before_writing_below() {
    let mut t = tab();
    let v = view(&mut t);
    v.pkg.workbook.sheets[0].set_col_width(0, 10.0);
    put(v, "A1", Cell::text("the quick brown"));
    put(v, "A2", Cell::text("fox jumps"));
    v.anchor = at("A1");
    v.sel = at("A3");
    assert_eq!(v.justify(false), Ok(true));
    assert_eq!(shown(v, "A1"), CellValue::Text("the quick".into()));
    assert_eq!(shown(v, "A2"), CellValue::Text("brown fox".into()));
    assert_eq!(shown(v, "A3"), CellValue::Text("jumps".into()));
    // Two rows cannot hold three lines: Excel asks.
    put(v, "A1", Cell::text("the quick brown fox jumps"));
    put(v, "A2", Cell::default());
    put(v, "A3", Cell::default());
    v.anchor = at("A1");
    v.sel = at("A2");
    assert_eq!(v.justify(false), Err(JUSTIFY_OVERFLOW.to_string()));
    t.dialogs.push(justify_question());
    assert_eq!(justify_click(&mut t, "OK"), Some(Ok(())));
    assert_eq!(shown(view(&mut t), "A3"), CellValue::Text("jumps".into()));
}

#[test]
fn custom_lists_add_import_delete_and_ok() {
    let mut t = tab();
    t.dialogs.push(custom_lists_dialog(
        &[vec!["x".into(), "y".into()]],
        vec!["North".into(), "South".into()],
    ));
    let items = |t: &DocTab| t.dialogs.top().unwrap().controls[0].items.clone();
    assert_eq!(items(&t).len(), 6, "NEW LIST, four built-ins, one of ours");
    set(&mut t, "entries", Value::Text("Low\nMid\nHigh".into()));
    assert_eq!(custom_lists_click(&mut t, "Add"), Some(Ok(None)));
    assert_eq!(custom_lists_click(&mut t, "Import"), Some(Ok(None)));
    assert_eq!(items(&t).last().unwrap(), "North, South");
    // A built-in cannot be deleted.
    set(&mut t, "lists", Value::Choice(Some(1)));
    assert!(matches!(custom_lists_click(&mut t, "Delete"), Some(Err(_))));
    set(&mut t, "lists", Value::Choice(Some(FIRST_USER)));
    assert_eq!(custom_lists_click(&mut t, "Delete"), Some(Ok(None)));
    let Some(Ok(Some(lists))) = custom_lists_click(&mut t, "OK") else {
        panic!("OK hands the lists back")
    };
    assert_eq!(
        lists,
        vec![
            vec!["Low".to_string(), "Mid".into(), "High".into()],
            vec!["North".to_string(), "South".into()]
        ]
    );
    assert!(t.dialogs.top().is_none());
}

#[test]
fn a_fill_continues_a_custom_list() {
    let mut t = tab();
    let v = view(&mut t);
    put(v, "A1", Cell::text("Mid"));
    let lists = vec![vec!["Low".to_string(), "Mid".into(), "High".into()]];
    v.fill_drag(&req("A1", "A3", FillKind::Auto, &lists))
        .unwrap();
    assert_eq!(shown(v, "A2"), CellValue::Text("High".into()));
    assert_eq!(shown(v, "A3"), CellValue::Text("Low".into()));
}
