//! Word's Mark as Final (#617): File > Info > Protect Document > Mark as
//! Final stores the custom property `_MarkAsFinal` = `true` in
//! `docProps/custom.xml`. It is advice, not `w:documentProtection`: a host
//! opens such a document read-only until the user chooses Edit Anyway, which
//! removes the property so a later save writes an ordinary document.
//!
//! Only `_MarkAsFinal` is read. `cp:contentStatus` ("Final", which Word also
//! sets) is a label, so it is left as it is.

use crate::inspect::find_element_from;
use crate::package::{Package, tag_attr};

const CUSTOM_PART: &str = "docProps/custom.xml";
const NAME: &str = "_MarkAsFinal";

/// The byte range of the `_MarkAsFinal` property in custom.xml `xml`, and
/// whether it holds true (`<vt:bool>` of `true` or `1`, any case).
fn find_mark(xml: &str) -> Option<(usize, usize, bool)> {
    let mut from = 0;
    while let Some((start, end, _)) = find_element_from(xml, "property", from) {
        let element = &xml[start..end];
        let head_end = crate::inspect::tag_end(element)?;
        if tag_attr(&element[..head_end], "name").as_deref() == Some(NAME) {
            return Some((start, end, holds_true(&element[head_end..])));
        }
        from = end;
    }
    None
}

/// Whether a property's content (after its start tag) is a true `vt:bool`.
fn holds_true(inner: &str) -> bool {
    let Some((start, end, _)) = find_element_from(inner, "vt:bool", 0) else {
        return false;
    };
    let element = &inner[start..end];
    let Some(head_end) = crate::inspect::tag_end(element) else {
        return false;
    };
    let value = element[head_end..]
        .strip_suffix("</vt:bool>")
        .unwrap_or_default()
        .trim();
    value.eq_ignore_ascii_case("true") || value == "1"
}

impl Package {
    /// Whether Word marked this document as final. Lenient: a missing or
    /// unreadable custom.xml, or any other value, is not final.
    pub fn marked_final(&self) -> bool {
        self.part_text(CUSTOM_PART)
            .and_then(|xml| find_mark(&xml))
            .is_some_and(|(_, _, on)| on)
    }

    /// Edit Anyway: remove the `_MarkAsFinal` property, keeping custom.xml
    /// and its other properties. Returns whether anything changed.
    pub fn clear_marked_final(&mut self) -> bool {
        let Some(xml) = self.part_text(CUSTOM_PART) else {
            return false;
        };
        let Some((start, end, _)) = find_mark(&xml) else {
            return false;
        };
        let out = format!("{}{}", &xml[..start], &xml[end..]);
        self.set_part_text(CUSTOM_PART, &out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::load_package;
    use crate::zipwrite::write_zip;

    const OTHER: &str = r#"<property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="3" name="Client"><vt:lpwstr>Acme</vt:lpwstr></property>"#;

    fn mark(value: &str) -> String {
        format!(
            r#"<property fmtid="{{D5CDD505-2E9C-101B-9397-08002B2CF9AE}}" pid="2" name="_MarkAsFinal"><vt:bool>{value}</vt:bool></property>"#
        )
    }

    fn custom(props: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes">{props}</Properties>"#
        )
    }

    fn package(custom_xml: Option<&str>) -> Package {
        let mut parts = vec![
            (
                "[Content_Types].xml".to_string(),
                br#"<?xml version="1.0"?><Types/>"#.to_vec(),
            ),
            (
                "_rels/.rels".to_string(),
                br#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "word/document.xml".to_string(),
                br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Final text.</w:t></w:r></w:p></w:body></w:document>"#.to_vec(),
            ),
        ];
        if let Some(xml) = custom_xml {
            parts.push((CUSTOM_PART.to_string(), xml.as_bytes().to_vec()));
        }
        load_package(&write_zip(&parts)).unwrap()
    }

    #[test]
    fn a_true_mark_is_final() {
        for value in ["true", "TRUE", "True", "1", " true "] {
            let pkg = package(Some(&custom(&format!("{OTHER}{}", mark(value)))));
            assert!(pkg.marked_final(), "{value}");
        }
    }

    #[test]
    fn anything_else_is_not_final() {
        for xml in [
            None,
            Some(custom(&mark("false"))),
            Some(custom(&mark("0"))),
            Some(custom(&mark(""))),
            Some(custom(OTHER)),
            // The name must match exactly.
            Some(custom(
                &mark("true").replace("_MarkAsFinal", "_MarkAsFinalX"),
            )),
            // A string "true" is not Word's boolean.
            Some(custom(&mark("x").replace(
                "<vt:bool>x</vt:bool>",
                "<vt:lpwstr>true</vt:lpwstr>",
            ))),
            Some("not xml at all <property".to_string()),
        ] {
            assert!(!package(xml.as_deref()).marked_final(), "{xml:?}");
        }
    }

    #[test]
    fn clearing_removes_only_the_mark() {
        let mut pkg = package(Some(&custom(&format!("{}{OTHER}", mark("true")))));
        assert!(pkg.clear_marked_final());
        assert!(!pkg.marked_final());
        assert_eq!(pkg.part_text(CUSTOM_PART).unwrap(), custom(OTHER));
        // Nothing left to clear.
        assert!(!pkg.clear_marked_final());
    }

    #[test]
    fn clearing_without_a_mark_changes_nothing() {
        let mut pkg = package(Some(&custom(OTHER)));
        assert!(!pkg.clear_marked_final());
        assert_eq!(pkg.part_text(CUSTOM_PART).unwrap(), custom(OTHER));
        assert!(!package(None).clear_marked_final());
    }

    #[test]
    fn a_cleared_mark_stays_cleared_through_a_save() {
        let mut pkg = package(Some(&custom(&mark("true"))));
        pkg.clear_marked_final();
        let saved = crate::package::save_package_preserving_document(&pkg);
        assert!(!load_package(&saved).unwrap().marked_final());
        // And an uncleared one survives a save.
        let pkg = package(Some(&custom(&mark("true"))));
        let saved = crate::package::save_package_preserving_document(&pkg);
        assert!(load_package(&saved).unwrap().marked_final());
    }

    #[test]
    fn the_denial_names_edit_anyway() {
        let denial = crate::protection::ProtectionDenial::MarkedFinal;
        assert_eq!(
            denial.control_error(),
            "protection_denied:marked_final: the document is marked as final; use Edit Anyway to edit it"
        );
    }
}
