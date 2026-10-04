use super::*;
use crate::sheet::{DataValidation, Sheet, Xf, parse_cell_name};

fn book(sheets: &[(&str, &[(&str, Cell)])]) -> Workbook {
    let sheets = sheets
        .iter()
        .map(|(name, cells)| {
            let mut sheet = Sheet {
                name: name.to_string(),
                ..Sheet::default()
            };
            for (at, cell) in cells.iter() {
                let (r, c) = parse_cell_name(at).unwrap();
                sheet.set_cell(r, c, cell.clone());
            }
            sheet
        })
        .collect();
    let mut wb = Workbook {
        sheets,
        ..Workbook::default()
    };
    wb.styles.xfs.push(Xf::default());
    wb
}

fn at(name: &str) -> (u32, u32) {
    parse_cell_name(name).unwrap()
}

fn cell(wb: &Workbook, s: usize, name: &str) -> Cell {
    let (r, c) = at(name);
    wb.sheets[s].cell(r, c).cloned().unwrap_or_default()
}

fn formula(wb: &Workbook, s: usize, name: &str) -> Option<String> {
    cell(wb, s, name).formula
}

fn value(wb: &Workbook, s: usize, name: &str) -> CellValue {
    cell(wb, s, name).value
}

/// A copy of the rectangle `r` ("A1:B2") on sheet `s`.
fn copy(wb: &Workbook, s: usize, r: &str) -> ClipBlock {
    let (a, b) = r.split_once(':').unwrap_or((r, r));
    let (r0, c0) = at(a);
    let (r1, c1) = at(b);
    ClipBlock::capture(wb, s, (r0..=r1).collect(), (c0..=c1).collect())
}

fn paste(wb: &mut Workbook, s: usize, to: &str, clip: &ClipBlock, spec: PasteSpec) -> Pasted {
    paste_special(wb, s, at(to), clip, &spec).unwrap()
}

fn op(o: PasteOp) -> PasteSpec {
    PasteSpec {
        op: o,
        ..PasteSpec::default()
    }
}

// ---- what --------------------------------------------------------------------

#[test]
fn values_paste_a_formulas_result_in_the_destinations_format() {
    let mut wb = book(&[("Sheet1", &[("A1", Cell::number(2.0))])]);
    let pct = wb.styles.intern(Xf {
        code: Some("0%".into()),
        ..Xf::default()
    });
    let bold = wb.styles.intern(Xf {
        bold: true,
        ..Xf::default()
    });
    let mut f = Cell::formula("A1*2");
    f.value = CellValue::Number(4.0);
    f.style = bold;
    wb.sheets[0].set_cell(0, 1, f);
    let mut dest = Cell::number(0.0);
    dest.style = pct;
    wb.sheets[0].set_cell(0, 3, dest);
    let clip = copy(&wb, 0, "B1");
    paste(&mut wb, 0, "D1", &clip, PasteSpec::of(PasteWhat::Values));
    let d1 = cell(&wb, 0, "D1");
    assert_eq!(
        (d1.value, d1.formula, d1.style),
        (CellValue::Number(4.0), None, pct)
    );
    // Formulas: the formula, translated; the destination's format.
    paste(&mut wb, 0, "D1", &clip, PasteSpec::of(PasteWhat::Formulas));
    let d1 = cell(&wb, 0, "D1");
    assert_eq!((d1.formula.as_deref(), d1.style), (Some("C1*2"), pct));
    // Formats: the source's style, the destination's value.
    wb.sheets[0].set_cell(0, 4, Cell::text("keep"));
    paste(&mut wb, 0, "E1", &clip, PasteSpec::of(PasteWhat::Formats));
    let e1 = cell(&wb, 0, "E1");
    assert_eq!((e1.value, e1.style), (CellValue::Text("keep".into()), bold));
    // All: everything.
    paste(&mut wb, 0, "F1", &clip, PasteSpec::of(PasteWhat::All));
    let f1 = cell(&wb, 0, "F1");
    assert_eq!((f1.formula.as_deref(), f1.style), (Some("E1*2"), bold));
}

#[test]
fn number_formats_come_with_formulas_and_values_when_asked() {
    let mut wb = book(&[("Sheet1", &[])]);
    let money = wb.styles.intern(Xf {
        code: Some("$#,##0.00".into()),
        bold: true,
        ..Xf::default()
    });
    let italic = wb.styles.intern(Xf {
        italic: true,
        ..Xf::default()
    });
    let mut src = Cell::number(5.0);
    src.style = money;
    wb.sheets[0].set_cell(0, 0, src);
    let mut d = Cell::default();
    d.style = italic;
    wb.sheets[0].set_cell(0, 2, d);
    let clip = copy(&wb, 0, "A1");
    paste(
        &mut wb,
        0,
        "C1",
        &clip,
        PasteSpec::of(PasteWhat::ValuesAndNumberFormats),
    );
    let xf = wb.styles.xf(cell(&wb, 0, "C1").style);
    assert_eq!(xf.code.as_deref(), Some("$#,##0.00"));
    assert!(
        xf.italic && !xf.bold,
        "the destination's font, the source's format"
    );
}

#[test]
fn all_except_borders_takes_the_borders_off() {
    let mut wb = book(&[("Sheet1", &[])]);
    let boxed = wb.styles.intern(Xf {
        border: true,
        bold: true,
        ..Xf::default()
    });
    let mut src = Cell::number(1.0);
    src.style = boxed;
    wb.sheets[0].set_cell(0, 0, src);
    let clip = copy(&wb, 0, "A1");
    paste(
        &mut wb,
        0,
        "B1",
        &clip,
        PasteSpec::of(PasteWhat::AllExceptBorders),
    );
    let xf = wb.styles.xf(cell(&wb, 0, "B1").style);
    assert!(xf.bold && !xf.border);
}

#[test]
fn column_widths_and_keep_source_column_widths() {
    let mut wb = book(&[("Sheet1", &[("A1", Cell::number(1.0))])]);
    wb.sheets[0].set_col_width(0, 30.0);
    let clip = copy(&wb, 0, "A1");
    paste(
        &mut wb,
        0,
        "C1",
        &clip,
        PasteSpec::of(PasteWhat::ColumnWidths),
    );
    assert_eq!(wb.sheets[0].col_width(2), 30.0);
    assert_eq!(value(&wb, 0, "C1"), CellValue::Empty, "widths only");
    paste(
        &mut wb,
        0,
        "E1",
        &clip,
        PasteSpec::of(PasteWhat::AllAndColumnWidths),
    );
    assert_eq!(wb.sheets[0].col_width(4), 30.0);
    assert_eq!(value(&wb, 0, "E1"), CellValue::Number(1.0));
}

#[test]
fn notes_come_back_for_the_package_to_write() {
    let wb = book(&[("Sheet1", &[("A1", Cell::number(1.0))])]);
    let mut clip = copy(&wb, 0, "A1:B1");
    clip.set_notes([
        (0, 1, "Ann".to_string(), "hi".to_string()),
        (5, 5, "Bob".to_string(), "not copied".to_string()),
    ]);
    let mut w = wb.clone();
    let p = paste(&mut w, 0, "C3", &clip, PasteSpec::of(PasteWhat::Comments));
    assert_eq!(p.notes, vec![(2, 3, "Ann".to_string(), "hi".to_string())]);
    assert_eq!(value(&w, 0, "C3"), CellValue::Empty, "notes only");
}

#[test]
fn validation_replaces_the_destinations_and_comes_back_as_rules() {
    let mut wb = book(&[("Sheet1", &[])]);
    wb.sheets[0].validations.push(DataValidation {
        ranges: vec![(0, 0, 1, 0)],
        kind: "list".into(),
        operator: String::new(),
        formula1: "\"a,b\"".into(),
        formula2: String::new(),
        prompt: None,
        ix: None,
    });
    wb.sheets[0].validations.push(DataValidation {
        ranges: vec![(0, 2, 5, 2)],
        kind: "whole".into(),
        operator: "between".into(),
        formula1: "1".into(),
        formula2: "9".into(),
        prompt: None,
        ix: Some(0),
    });
    let clip = copy(&wb, 0, "A1:A2");
    let p = paste(
        &mut wb,
        0,
        "C2",
        &clip,
        PasteSpec::of(PasteWhat::Validation),
    );
    assert_eq!(p.rules.len(), 1);
    assert_eq!(p.rules[0].0, (1, 2, 2, 2));
    assert_eq!(p.rules[0].1.kind, "list");
    // The whole-number rule lost C2:C3 and kept the rest.
    assert_eq!(
        wb.sheets[0].validations[1].ranges,
        vec![(0, 2, 0, 2), (3, 2, 5, 2)]
    );
    // A rule cleared away entirely is named for the save.
    let mut w = wb.clone();
    clear_validation(&mut w.sheets[0], (0, 2, 9, 2));
    assert_eq!(w.sheets[0].validations.len(), 1);
    assert_eq!(w.sheets[0].dv_removed, vec![0]);
    // Formulas don't touch validation.
    let mut w = wb.clone();
    let p = paste(&mut w, 0, "C2", &clip, PasteSpec::of(PasteWhat::Formulas));
    assert!(p.rules.is_empty());
    assert_eq!(
        w.sheets[0].validations[1].ranges,
        wb.sheets[0].validations[1].ranges
    );
}

// ---- operations (R10) -----------------------------------------------------------

#[test]
fn constants_combine_into_the_number() {
    let mut wb = book(&[(
        "Sheet1",
        &[("A1", Cell::number(4.0)), ("B1", Cell::number(10.0))],
    )]);
    let clip = copy(&wb, 0, "A1");
    for (o, want) in [
        (PasteOp::Add, 14.0),
        (PasteOp::Subtract, 6.0),
        (PasteOp::Multiply, 40.0),
        (PasteOp::Divide, 2.5),
    ] {
        let mut w = wb.clone();
        paste(&mut w, 0, "B1", &clip, op(o));
        assert_eq!(value(&w, 0, "B1"), CellValue::Number(want), "{o:?}");
    }
    // A blank destination is 0.
    paste(&mut wb, 0, "C1", &clip, op(PasteOp::Subtract));
    assert_eq!(value(&wb, 0, "C1"), CellValue::Number(-4.0));
}

#[test]
fn a_blank_source_is_zero() {
    let wb = book(&[("Sheet1", &[("B1", Cell::number(10.0))])]);
    let clip = copy(&wb, 0, "A1");
    let mut w = wb.clone();
    paste(&mut w, 0, "B1", &clip, op(PasteOp::Add));
    assert_eq!(value(&w, 0, "B1"), CellValue::Number(10.0));
    let mut w = wb.clone();
    paste(&mut w, 0, "B1", &clip, op(PasteOp::Multiply));
    assert_eq!(value(&w, 0, "B1"), CellValue::Number(0.0));
    let mut w = wb.clone();
    paste(&mut w, 0, "B1", &clip, op(PasteOp::Divide));
    assert_eq!(value(&w, 0, "B1"), CellValue::Error("#DIV/0!".into()));
}

#[test]
fn a_destination_formula_is_wrapped() {
    let mut wb = book(&[("Sheet1", &[("A1", Cell::number(1.05))])]);
    wb.sheets[0].set_cell(8, 2, Cell::formula("B9*2"));
    let clip = copy(&wb, 0, "A1");
    paste(&mut wb, 0, "C9", &clip, op(PasteOp::Multiply));
    assert_eq!(formula(&wb, 0, "C9").as_deref(), Some("(B9*2)*1.05"));
}

#[test]
fn a_source_formula_is_wrapped_after_the_destination() {
    let mut wb = book(&[("Sheet1", &[("C1", Cell::number(10.0))])]);
    wb.sheets[0].set_cell(0, 1, Cell::formula("A1"));
    let clip = copy(&wb, 0, "B1");
    // B1 =A1 pasted at C1 reads B1.
    paste(&mut wb, 0, "C1", &clip, op(PasteOp::Add));
    assert_eq!(formula(&wb, 0, "C1").as_deref(), Some("10+(B1)"));
    // Onto a blank: 0 op s, unsimplified.
    paste(&mut wb, 0, "D1", &clip, op(PasteOp::Add));
    assert_eq!(formula(&wb, 0, "D1").as_deref(), Some("0+(C1)"));
}

#[test]
fn two_formulas_are_both_wrapped() {
    let mut wb = book(&[("Sheet1", &[])]);
    wb.sheets[0].set_cell(0, 0, Cell::formula("Z1"));
    wb.sheets[0].set_cell(8, 2, Cell::formula("B9*2"));
    let clip = copy(&wb, 0, "A1");
    paste(&mut wb, 0, "C9", &clip, op(PasteOp::Multiply));
    assert_eq!(formula(&wb, 0, "C9").as_deref(), Some("(B9*2)*(AB9)"));
}

#[test]
fn text_on_either_side_is_left_alone() {
    let wb = book(&[(
        "Sheet1",
        &[
            ("A1", Cell::text("x")),
            ("A2", Cell::number(3.0)),
            ("B1", Cell::number(10.0)),
            ("B2", Cell::text("y")),
        ],
    )]);
    let clip = copy(&wb, 0, "A1:A2");
    let mut w = wb.clone();
    paste(&mut w, 0, "B1", &clip, op(PasteOp::Add));
    assert_eq!(value(&w, 0, "B1"), CellValue::Number(10.0));
    assert_eq!(value(&w, 0, "B2"), CellValue::Text("y".into()));
}

// ---- skip blanks and transpose ---------------------------------------------------

#[test]
fn skip_blanks_leaves_the_destination() {
    let wb = book(&[(
        "Sheet1",
        &[
            ("A1", Cell::number(1.0)),
            ("A3", Cell::number(3.0)),
            ("C2", Cell::text("kept")),
        ],
    )]);
    let clip = copy(&wb, 0, "A1:A3");
    let mut w = wb.clone();
    paste(
        &mut w,
        0,
        "C1",
        &clip,
        PasteSpec {
            skip_blanks: true,
            ..PasteSpec::default()
        },
    );
    assert_eq!(value(&w, 0, "C2"), CellValue::Text("kept".into()));
    assert_eq!(value(&w, 0, "C3"), CellValue::Number(3.0));
    let mut w = wb.clone();
    paste(&mut w, 0, "C1", &clip, PasteSpec::default());
    assert_eq!(value(&w, 0, "C2"), CellValue::Empty);
}

#[test]
fn transpose_repoints_references_into_the_copy() {
    // R1: A1=1, A2==A1+1 pasted transposed at C1 gives D1 = C1+1, and a
    // reference outside the copy moves with its cell: A2's B5 → E4.
    let mut wb = book(&[("Sheet1", &[("A1", Cell::number(1.0))])]);
    wb.sheets[0].set_cell(1, 0, Cell::formula("A1+1+B5"));
    let clip = copy(&wb, 0, "A1:A2");
    let p = paste(
        &mut wb,
        0,
        "C1",
        &clip,
        PasteSpec {
            transpose: true,
            ..PasteSpec::default()
        },
    );
    assert_eq!(p.rect, (0, 2, 0, 3));
    assert_eq!(value(&wb, 0, "C1"), CellValue::Number(1.0));
    assert_eq!(formula(&wb, 0, "D1").as_deref(), Some("C1+1+E4"));
}

#[test]
fn transpose_maps_a_range_inside_the_copy_corner_by_corner() {
    let mut wb = book(&[(
        "Sheet1",
        &[("A1", Cell::number(1.0)), ("A2", Cell::number(2.0))],
    )]);
    wb.sheets[0].set_cell(2, 0, Cell::formula("SUM(A1:A2)+$A$1"));
    let clip = copy(&wb, 0, "A1:A3");
    paste(
        &mut wb,
        0,
        "C1",
        &clip,
        PasteSpec {
            transpose: true,
            ..PasteSpec::default()
        },
    );
    assert_eq!(formula(&wb, 0, "E1").as_deref(), Some("SUM(C1:D1)+$C$1"));
}

// ---- multi-area copy -------------------------------------------------------------

#[test]
fn a_multi_area_copy_needs_shared_rows_or_columns() {
    assert_eq!(
        multi_area_shape(&[(0, 0, 2, 0), (0, 2, 2, 2)]),
        Ok((vec![0, 1, 2], vec![0, 2]))
    );
    assert_eq!(
        multi_area_shape(&[(4, 1, 4, 2), (0, 1, 1, 2)]),
        Ok((vec![0, 1, 4], vec![1, 2]))
    );
    assert_eq!(
        multi_area_shape(&[(0, 0, 0, 0), (2, 2, 2, 2)]),
        Err(MULTI_SELECTION)
    );
}

#[test]
fn a_multi_area_copy_translates_each_column_from_its_own_source() {
    // R2: A1:A3 + C1:C3 with C1 = B1, pasted at E1: the second block column
    // is F, and C's formula moved three columns, so F1 = E1.
    let mut wb = book(&[("Sheet1", &[("A1", Cell::number(1.0))])]);
    wb.sheets[0].set_cell(0, 2, Cell::formula("B1"));
    let (rows, cols) = multi_area_shape(&[(0, 0, 2, 0), (0, 2, 2, 2)]).unwrap();
    let clip = ClipBlock::capture(&wb, 0, rows, cols);
    paste(&mut wb, 0, "E1", &clip, PasteSpec::default());
    assert_eq!(value(&wb, 0, "E1"), CellValue::Number(1.0));
    assert_eq!(formula(&wb, 0, "F1").as_deref(), Some("E1"));
}

// ---- paste link ------------------------------------------------------------------

#[test]
fn paste_link_is_absolute_for_one_cell_and_relative_for_a_range() {
    let wb = book(&[
        ("Sheet1", &[("A1", Cell::number(1.0))]),
        ("Other Sheet", &[]),
    ]);
    let one = copy(&wb, 0, "A1");
    let ch = paste_link_changes(&wb, 0, at("C3"), &one);
    assert_eq!(ch.len(), 1);
    assert_eq!(ch[0].2.formula.as_deref(), Some("$A$1"));
    let range = copy(&wb, 0, "A1:B2");
    let ch = paste_link_changes(&wb, 0, at("D1"), &range);
    let fs: Vec<_> = ch
        .iter()
        .map(|(r, c, cell)| (cell_name(*r, *c), cell.formula.clone().unwrap()))
        .collect();
    assert_eq!(
        fs,
        [
            ("D1".to_string(), "A1".to_string()),
            ("E1".into(), "B1".into()),
            ("D2".into(), "A2".into()),
            ("E2".into(), "B2".into())
        ]
    );
    // From another sheet, qualified.
    let ch = paste_link_changes(&wb, 1, at("A1"), &one);
    assert_eq!(ch[0].2.formula.as_deref(), Some("Sheet1!$A$1"));
    let other = copy(&wb, 1, "A1");
    let ch = paste_link_changes(&wb, 0, at("A1"), &other);
    assert_eq!(ch[0].2.formula.as_deref(), Some("'Other Sheet'!$A$1"));
}

#[test]
fn labels_round_trip() {
    for w in PasteWhat::DIALOG {
        assert_eq!(PasteWhat::from_label(w.label()), Some(w));
    }
    assert_eq!(PasteWhat::from_label("notes"), Some(PasteWhat::Comments));
    for o in PasteOp::ALL {
        assert_eq!(PasteOp::from_label(o.label()), Some(o));
    }
}

#[test]
fn cells_become_rectangles() {
    let cells = [(0, 0), (0, 1), (1, 0), (1, 1), (3, 0), (1, 3)];
    assert_eq!(
        cells_to_rects(&cells),
        vec![(0, 0, 1, 1), (1, 3, 1, 3), (3, 0, 3, 0)]
    );
    assert_eq!(subtract((0, 0, 4, 4), (1, 1, 2, 2)).len(), 4);
    assert_eq!(subtract((0, 0, 0, 0), (5, 5, 5, 5)), vec![(0, 0, 0, 0)]);
}
