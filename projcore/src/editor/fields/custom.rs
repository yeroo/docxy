//! Custom task fields (Text1-30, Number1-20, Cost1-10, Flag1-20 and
//! Date/Start/Finish/Duration1-10, #577): each field resolves to a
//! `FieldID` through the plan's own `<ExtendedAttribute>` definitions — the
//! definition whose `FieldName` is the field's name says which `FieldID`
//! holds the value — so no id is hard-coded. A field with no definition in
//! the plan, or no task value with that id, reads as unset; a stored value
//! that does not parse reads as its raw text and [`FieldValue::Null`], never
//! an error.
use super::*;
use crate::model::{ExtendedAttributeValue, XmlElement};

/// Read one custom field of one task.
pub(super) fn read(proj: &Project, task: &Task, kind: CustomKind, n: u8) -> FieldRead {
    let name = Field::Custom(kind, n).name();
    let Some(definition) = proj
        .extended_attribute_definitions
        .iter()
        .find(|def| child(def, "FieldName").is_some_and(|f| f.eq_ignore_ascii_case(&name)))
    else {
        return unset(proj, kind);
    };
    let Some(id) = child(definition, "FieldID") else {
        return unset(proj, kind);
    };
    let Some(value) = task
        .extended_attributes
        .iter()
        .find(|value| value.field_id.trim() == id)
    else {
        return unset(proj, kind);
    };
    value_read(proj, definition, value, kind)
}

/// What a field with no resolvable value reads: what Project shows for an
/// unset custom field, with Null underneath (Flag excepted: a false flag
/// reads `No`/`false`, like the built-in flags).
fn unset(proj: &Project, kind: CustomKind) -> FieldRead {
    match kind {
        CustomKind::Text => FieldRead::new("", FieldValue::Null),
        CustomKind::Number => FieldRead::new("0", FieldValue::Null),
        CustomKind::Cost => FieldRead::new(format_money(0.0), FieldValue::Null),
        CustomKind::Flag => flag(false),
        CustomKind::Date | CustomKind::Start | CustomKind::Finish => {
            FieldRead::new("NA", FieldValue::Null)
        }
        CustomKind::Duration => duration(proj, None, DurationUnit::DAYS),
    }
}

/// Format a stored value by kind; a value that does not parse reads as its
/// raw text and Null.
fn value_read(
    proj: &Project,
    definition: &XmlElement,
    value: &ExtendedAttributeValue,
    kind: CustomKind,
) -> FieldRead {
    let raw = value.value.as_deref().unwrap_or("");
    match kind {
        CustomKind::Text => match raw {
            // A lookup-table text field may store no value and name its
            // entry by guid; the entry's Value is the text.
            "" => match value
                .value_guid
                .as_deref()
                .and_then(|guid| lookup_value_list(definition, guid))
            {
                Some(found) => text(found),
                None => FieldRead::new("", FieldValue::Null),
            },
            _ => text(raw),
        },
        CustomKind::Number => match raw.trim().parse::<f64>() {
            Ok(number) => {
                let (shown, _) = two_decimals(number);
                FieldRead::new(shown, FieldValue::Number(number))
            }
            Err(_) => FieldRead::new(raw, FieldValue::Null),
        },
        // MSPDI stores every cost in hundredths, like the built-in costs.
        CustomKind::Cost => match raw.trim().parse::<f64>() {
            Ok(hundredths) => money_value(Some(hundredths)),
            Err(_) => FieldRead::new(raw, FieldValue::Null),
        },
        CustomKind::Flag => {
            let trimmed = raw.trim();
            if trimmed == "1" || trimmed.eq_ignore_ascii_case("true") {
                flag(true)
            } else if trimmed == "0" || trimmed.eq_ignore_ascii_case("false") {
                flag(false)
            } else {
                FieldRead::new(raw, FieldValue::Null)
            }
        }
        CustomKind::Date | CustomKind::Start | CustomKind::Finish => {
            match DateTime::parse_mspdi(raw) {
                Some(dt) => date(Some(dt)),
                None => FieldRead::new(raw, FieldValue::Null),
            }
        }
        CustomKind::Duration => {
            let unit = value
                .duration_format
                .and_then(DurationUnit::of_code)
                .unwrap_or(DurationUnit::DAYS);
            match crate::mspdi::try_iso8601_to_minutes(raw) {
                Some(min) => duration(proj, Some(min), unit),
                None => FieldRead::new(raw, FieldValue::Null),
            }
        }
    }
}

/// A definition child's trimmed text, when it has a child with this name.
fn child<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element
        .children
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.text.trim())
}

/// The `Value` of the definition's `ValueList` entry whose `FieldGUID`
/// matches the stored guid (ASCII case-insensitive).
fn lookup_value_list<'a>(definition: &'a XmlElement, guid: &str) -> Option<&'a str> {
    let list = definition.children.iter().find(|c| c.name == "ValueList")?;
    list.children
        .iter()
        .filter(|entry| entry.name == "Value")
        .find(|entry| {
            entry
                .children
                .iter()
                .any(|c| c.name == "FieldGUID" && c.text.trim().eq_ignore_ascii_case(guid.trim()))
        })
        .and_then(|entry| entry.children.iter().find(|c| c.name == "Value"))
        .map(|c| c.text.as_str())
}
