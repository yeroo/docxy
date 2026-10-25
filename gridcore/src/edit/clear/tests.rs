use super::*;
use crate::sheet::{CellValue, Workbook, Xf, parse_cell_name};

fn book() -> Workbook {
    let mut wb = Workbook::default();
    wb.styles.xfs.push(Xf::default());
    let bold = wb.styles.intern(Xf {
        bold: true,
        ..Xf::default()
    });
    let mut s = Sheet {
        name: "Sheet1".into(),
        ..Sheet::default()
    };
    let mut a1 = Cell::number(1.0);
    a1.style = bold;
    s.set_cell(0, 0, a1);
    s.set_cell(0, 1, Cell::formula("A1*2"));
    s.set_cell(5, 5, Cell::text("outside"));
    s.hyperlinks.insert((0, 0), "https://example.com".into());
    s.hyperlink_refs.insert(
        (0, 0),
        crate::sheet::LinkRef {
            loaded: (0, 0, 0, 0),
            now: (0, 0, 0, 0),
            target: "https://example.com".into(),
        },
    );
    wb.sheets.push(s);
    wb
}

fn cell(wb: &Workbook, name: &str) -> Cell {
    let (r, c) = parse_cell_name(name).unwrap();
    wb.sheets[0].cell(r, c).cloned().unwrap_or_default()
}

const A1B2: Area = (0, 0, 1, 1);
const NOTES: &[(u32, u32)] = &[(0, 0), (5, 5)];

#[test]
fn clear_all_takes_everything_but_moves_nothing() {
    let mut wb = book();
    let plan = apply_clear(&mut wb, 0, &[A1B2], ClearWhat::All, NOTES).unwrap();
    assert_eq!(cell(&wb, "A1"), Cell::default());
    assert_eq!(cell(&wb, "B1"), Cell::default());
    assert!(wb.sheets[0].hyperlinks.is_empty());
    assert_eq!(plan.notes, vec![(0, 0)]);
    assert_eq!(cell(&wb, "F6").value, CellValue::Text("outside".into()));
}

#[test]
fn clear_formats_keeps_contents_notes_and_links() {
    let mut wb = book();
    let plan = apply_clear(&mut wb, 0, &[A1B2], ClearWhat::Formats, NOTES).unwrap();
    let a1 = cell(&wb, "A1");
    assert_eq!((a1.value, a1.style), (CellValue::Number(1.0), 0));
    assert_eq!(wb.sheets[0].hyperlinks.len(), 1);
    assert!(plan.notes.is_empty());
}

#[test]
fn clear_contents_keeps_the_format() {
    let mut wb = book();
    let style = cell(&wb, "A1").style;
    apply_clear(&mut wb, 0, &[A1B2], ClearWhat::Contents, NOTES).unwrap();
    let a1 = cell(&wb, "A1");
    assert_eq!((a1.value, a1.style), (CellValue::Empty, style));
    assert_eq!(cell(&wb, "B1").formula, None);
    assert_eq!(wb.sheets[0].hyperlinks.len(), 1);
}

#[test]
fn clear_comments_only_names_the_notes() {
    let mut wb = book();
    let before = wb.sheets[0].cells.clone();
    let plan = apply_clear(&mut wb, 0, &[A1B2], ClearWhat::Comments, NOTES).unwrap();
    assert_eq!(plan.notes, vec![(0, 0)]);
    assert_eq!(wb.sheets[0].cells, before);
}

#[test]
fn clear_hyperlinks_keeps_the_style_and_remove_resets_it() {
    let mut wb = book();
    let style = cell(&wb, "A1").style;
    apply_clear(&mut wb, 0, &[A1B2], ClearWhat::Hyperlinks, NOTES).unwrap();
    assert!(wb.sheets[0].hyperlinks.is_empty());
    assert_eq!(cell(&wb, "A1").style, style);
    assert_eq!(wb.sheets[0].hyperlinks_removed, vec![(0, 0, 0, 0)]);
    let mut wb = book();
    apply_clear(&mut wb, 0, &[A1B2], ClearWhat::RemoveHyperlinks, NOTES).unwrap();
    assert!(wb.sheets[0].hyperlinks.is_empty());
    let a1 = cell(&wb, "A1");
    assert_eq!((a1.value, a1.style), (CellValue::Number(1.0), 0));
}

#[test]
fn every_area_is_cleared() {
    let mut wb = book();
    apply_clear(
        &mut wb,
        0,
        &[(0, 0, 0, 0), (5, 5, 5, 5)],
        ClearWhat::Contents,
        &[],
    )
    .unwrap();
    assert_eq!(cell(&wb, "A1").value, CellValue::Empty);
    assert_eq!(cell(&wb, "F6").value, CellValue::Empty);
    assert!(cell(&wb, "B1").formula.is_some(), "between the areas");
}

#[test]
fn merges_inside_go_and_a_split_one_is_refused() {
    let mut wb = book();
    wb.sheets[0].merges.push((0, 0, 0, 1));
    wb.sheets[0].merges.push((4, 4, 6, 6));
    assert_eq!(
        apply_clear(&mut wb, 0, &[A1B2], ClearWhat::All, &[]).map(|p| p.unmerge),
        Ok(vec![(0, 0, 0, 1)])
    );
    assert_eq!(wb.sheets[0].merges, vec![(4, 4, 6, 6)]);
    let before = wb.clone();
    assert_eq!(
        apply_clear(&mut wb, 0, &[(5, 5, 5, 5)], ClearWhat::Formats, &[]),
        Err(MERGED_PART)
    );
    assert_eq!(wb.sheets[0].cells, before.sheets[0].cells);
    // Contents leave merges alone.
    assert!(apply_clear(&mut wb, 0, &[(5, 5, 5, 5)], ClearWhat::Contents, &[]).is_ok());
}

#[test]
fn labels_round_trip() {
    for w in ClearWhat::ALL {
        assert_eq!(ClearWhat::from_label(w.label()), Some(w));
    }
    assert_eq!(ClearWhat::from_label("formats"), Some(ClearWhat::Formats));
}

/// #707 r6: a clear over 10,000 areas (5,000 whole-height column strips and
/// 5,000 single cells) of a sheet with 50,000 cells, links, notes and
/// merges looks the areas up through an index.
#[test]
fn clearing_10k_areas_over_50k_cells_links_notes_and_merges_is_fast() {
    let n = 50_000u32;
    let mut wb = book();
    let s = &mut wb.sheets[0];
    for r in 0..n {
        s.set_cell(r, 0, Cell::number(1.0));
        s.set_cell(r, 1, Cell::number(2.0));
        s.hyperlinks.insert((r, 0), "https://example.com".into());
        s.merges.push((r, 12_000, r, 12_001));
    }
    let mut areas: Vec<Area> = (0..5_000u32).map(|k| (0, 2 * k, n - 1, 2 * k)).collect();
    areas.extend((0..5_000u32).map(|k| (k * 10, 1, k * 10, 1)));
    let notes: Vec<(u32, u32)> = (0..n).map(|r| (r, 0)).collect();
    let t = std::time::Instant::now();
    let plan = clear_plan(&wb.sheets[0], &areas, ClearWhat::All, &notes).unwrap();
    assert!(t.elapsed() < crate::edit::PERF_BOUND, "{:?}", t.elapsed());
    // Column A whole and every tenth B (F6 is in no strip).
    assert_eq!(plan.cells.len(), 50_000 + 5_000);
    assert_eq!((plan.notes.len(), plan.unlink.len()), (50_000, 50_000));
    assert!(plan.unmerge.is_empty());
}

/// #707 r10 m1: Clear Formats over 10,000 scattered cells of a conditional
/// format's range splits each fragment only by the areas that meet it: fast,
/// and the fragments left cover exactly the cells not cleared.
#[test]
fn clearing_10k_areas_out_of_conditional_formatting_is_fast() {
    use crate::sheet::{CfKind, CfRule, CondFormat};
    let mut wb = book();
    wb.sheets[0].cond_formats.push(CondFormat {
        ranges: vec![(0, 0, 49_999, 25)],
        rules: vec![CfRule {
            kind: CfKind::Expression {
                formula: "A1>0".into(),
            },
            dxf_id: None,
            priority: 1,
        }],
        ix: None,
    });
    let areas: Vec<Area> = (0..10_000u32)
        .map(|k| {
            let (r, c) = (k * 5 + 1, (k * 7) % 26);
            (r, c, r, c)
        })
        .collect();
    let t = std::time::Instant::now();
    let plan = clear_plan(&wb.sheets[0], &areas, ClearWhat::Formats, &[]).unwrap();
    apply_clear_sheet(&mut wb.sheets[0], &plan);
    assert!(t.elapsed() < crate::edit::PERF_BOUND, "{:?}", t.elapsed());
    let cf = &wb.sheets[0].cond_formats[0];
    let size = |a: &Area| u64::from(a.2 - a.0 + 1) * u64::from(a.3 - a.1 + 1);
    assert_eq!(
        cf.ranges.iter().map(size).sum::<u64>(),
        50_000 * 26 - 10_000
    );
    let ix = crate::edit::areas::RectIndex::new(&areas);
    assert!(cf.ranges.iter().all(|&r| !ix.meets(r)));
    // The anchor stayed at A1: the formula reads as before.
    assert!(matches!(&cf.rules[0].kind, CfKind::Expression { formula } if formula == "A1>0"));
}

/// #1681: a classic `cfvo` of type `formula` follows the anchor a clear moved,
/// as the rule's `<formula>`s do.
#[test]
fn clear_formats_reanchors_a_classic_cfvo_formula() {
    use crate::sheet::{CfKind, CfRule, Cfvo, CondFormat};
    let mut wb = book();
    wb.sheets[0].cond_formats.push(CondFormat {
        ranges: vec![(0, 0, 9, 0)],
        rules: vec![CfRule {
            kind: CfKind::Other {
                rule_type: "dataBar".into(),
                formulas: vec!["B1".into()],
                cfvos: vec![
                    Cfvo {
                        kind: "formula".into(),
                        val: "C1".into(),
                        gte: true,
                    },
                    Cfvo {
                        kind: "num".into(),
                        val: "7".into(),
                        gte: true,
                    },
                ],
            },
            dxf_id: None,
            priority: 1,
        }],
        ix: None,
    });
    let plan = clear_plan(&wb.sheets[0], &[(0, 0, 2, 0)], ClearWhat::Formats, &[]).unwrap();
    apply_clear_sheet(&mut wb.sheets[0], &plan);
    let rule = &mut wb.sheets[0].cond_formats[0].rules[0];
    assert_eq!(rule.formulas(), ["B4"]);
    let vals: Vec<_> = rule.cfvos().iter().map(|v| v.val.clone()).collect();
    assert_eq!(vals, ["C4", "7"]);
}

/// #1681: coverage that only an Excel 2010 rule gives still plans an area to
/// uncover.
#[test]
fn clear_over_x14_only_coverage_plans_an_uncond_area() {
    use crate::sheet::{ExtKind, ExtRule};
    let mut wb = book();
    wb.sheets[0].ext_rules.push(ExtRule {
        kind: ExtKind::Cf,
        ranges: vec![(0, 0, 4, 0)],
        formulas: vec!["A1>0".into()],
        ix: 0,
    });
    wb.sheets[0].ext_rules.push(ExtRule {
        kind: ExtKind::Dv,
        ranges: vec![(0, 5, 4, 5)],
        formulas: vec!["Z1".into()],
        ix: 0,
    });
    let hit = clear_plan(&wb.sheets[0], &[(2, 0, 2, 0)], ClearWhat::Formats, &[]).unwrap();
    assert_eq!(hit.uncond, vec![(2, 0, 2, 0)]);
    // A validation is not conditional formatting; a miss plans nothing.
    let dv = clear_plan(&wb.sheets[0], &[(2, 5, 2, 5)], ClearWhat::Formats, &[]).unwrap();
    assert!(dv.uncond.is_empty());
}

/// #1734: an Excel 2010 rule a clear takes whole is named struck once, also
/// when `ext_removed` already names it (a load never gives that state, so it
/// is seeded): Clear Formats for conditional formatting, the validation clear
/// Paste Special and the dialog share for validations.
#[test]
fn an_x14_rule_already_named_struck_is_named_once() {
    use crate::sheet::{ExtKind, ExtRule};
    let mut wb = book();
    let s = &mut wb.sheets[0];
    for kind in [ExtKind::Cf, ExtKind::Dv] {
        s.ext_rules.push(ExtRule {
            kind,
            ranges: vec![(0, 0, 4, 0)],
            formulas: vec!["A1>0".into()],
            ix: 3,
        });
    }
    s.ext_removed = vec![(ExtKind::Cf, 3), (ExtKind::Dv, 3)];
    clear_cond_formats(s, &[(0, 0, 9, 0)]);
    assert_eq!(s.ext_rules.len(), 1, "the validation stays");
    assert_eq!(s.ext_removed, vec![(ExtKind::Cf, 3), (ExtKind::Dv, 3)]);
    crate::edit::clear_validation(s, (0, 0, 9, 0));
    assert!(s.ext_rules.is_empty());
    assert_eq!(s.ext_removed, vec![(ExtKind::Cf, 3), (ExtKind::Dv, 3)]);
}
