//! Mail merge (#628): a recipient list read from a CSV file, the merge fields
//! that draw on it (MERGEFIELD, ADDRESSBLOCK, GREETINGLINE, NEXT, MERGEREC,
//! MERGESEQ), previewing a record in place, merging into a new document, and
//! the envelope and label layouts Word's Mailings tab builds.
//!
//! Merge fields stay ordinary [`crate::model::Inline::Field`]s: `raw` is what
//! is saved, `text` is what is shown, so a preview only changes `text`.

pub mod csv;
pub mod envelope;
pub mod fields;
pub mod finish;
pub mod preview;

pub use csv::Recipients;
pub use fields::{
    AddressField, FieldMap, GreetingName, MergeContext, MergeFieldKind, address_block_field, eval,
    field_kind, greeting_line_field, instr_kind, merge_field, might_be_merge_field, rule_field,
};
pub use finish::{MergeOptions, MergeRange, merge_package, merge_rows};
pub use preview::MergePreview;
