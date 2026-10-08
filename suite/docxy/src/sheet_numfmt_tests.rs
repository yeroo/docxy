//! The Number Format box's name for a cell's format code (#1140).
//! `uiharness/cases/sheet-numfmt-box.uit` asserts it through `ribbon-read`.

use super::*;
use core::prelude::v1::test;

fn name_of_id(id: u32) -> &'static str {
    numfmt_category(gridcore::numfmt::builtin_code(id))
}

#[test]
fn numfmt_category_names_builtin_ids() {
    assert_eq!(name_of_id(0), "General");
    assert_eq!(name_of_id(2), "Number");
    assert_eq!(name_of_id(4), "Number");
    assert_eq!(name_of_id(9), "Percentage");
    assert_eq!(name_of_id(10), "Percentage");
    assert_eq!(name_of_id(11), "Scientific");
    assert_eq!(name_of_id(14), "Date");
    assert_eq!(name_of_id(21), "Time");
    assert_eq!(name_of_id(49), "Text");
}

#[test]
fn numfmt_category_every_builtin_code_is_a_category() {
    for id in 0..50 {
        if let Some(code) = gridcore::numfmt::builtin_code(id) {
            assert_ne!(numfmt_category(Some(code)), "Custom", "id {id}: {code}");
        }
    }
}

#[test]
fn numfmt_category_general_spellings() {
    assert_eq!(numfmt_category(None), "General");
    assert_eq!(numfmt_category(Some("")), "General");
    assert_eq!(numfmt_category(Some("General")), "General");
    assert_eq!(numfmt_category(Some("general")), "General");
    assert_eq!(numfmt_category(Some(" General ")), "General");
}

#[test]
fn numfmt_category_padded_codes_are_custom() {
    assert_eq!(numfmt_category(Some("0 ")), "Custom");
    assert_eq!(numfmt_category(Some(" 0.00")), "Custom");
    assert_eq!(numfmt_category(Some("  ")), "Custom");
}

#[test]
fn numfmt_category_fractions_and_custom() {
    assert_eq!(numfmt_category(Some("# ?/?")), "Fraction");
    assert_eq!(numfmt_category(Some("# ??/??")), "Fraction");
    assert_eq!(numfmt_category(Some("0.000\"kg\"")), "Custom");
}

#[test]
fn numfmt_category_picker_codes_keep_their_label() {
    for (label, code) in NUM_FORMATS {
        assert_eq!(numfmt_category((!code.is_empty()).then_some(code)), label);
    }
}
