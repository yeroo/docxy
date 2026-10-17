//! Custom task and assignment fields (Text1-30, Number1-20, Cost1-10,
//! Flag1-20 and Date/Start/Finish/Duration1-10, #577, #1178): each field
//! resolves to a `FieldID` through the plan's own `<ExtendedAttribute>`
//! definitions, so no id is hard-coded. The definitions block is shared with
//! resource and assignment custom fields, whose `FieldName`s repeat the task
//! fields', so the first definition whose `FieldName` matches and whose
//! `FieldID` names one of the owner's values wins. Project's Custom Fields
//! dialog defines no assignment fields, so an assignment also resolves through
//! a matching definition's `SecondaryPID`, read as the id of the paired
//! assignment field (task Text1's is `0x0F40400A`, `0x0F40` the assignment
//! table). That reading of the schema is ours and unverified in Project; it
//! only resolves when an assignment value carries exactly that id. A field
//! with no such definition, or no value with that id, reads as unset; a stored
//! value that does not parse reads as its raw text and [`FieldValue::Null`],
//! never an error.
use super::*;
use crate::model::{ExtendedAttributeValue, XmlElement};

/// Read one custom field of one task.
pub(super) fn read(proj: &Project, task: &Task, kind: CustomKind, n: u8) -> FieldRead {
    // The definitions block is shared: it also holds resource and assignment
    // definitions, whose FieldName repeats the task fields' (resource Text1
    // is FieldID 205520904, task Text1 188743731). Resolve across every
    // definition with a matching FieldName: the first whose FieldID names a
    // task value wins, so an earlier definition for another owner — or with
    // no FieldID child — cannot shadow the task's. SecondaryPID is never
    // consulted for a task.
    let resolved = definitions(proj, kind, n).find_map(|def| {
        Some((
            def,
            by_id(&task.extended_attributes, child(def, "FieldID")?)?,
        ))
    });
    match resolved {
        Some((definition, value)) => value_read(proj, definition, value, kind),
        None => unset(proj, kind),
    }
}

/// Read one custom field of one assignment: the first matching definition
/// whose own `FieldID` names one of its values (normally an assignment
/// definition, but any owner's counts), else the first whose `SecondaryPID`
/// does. Among the definitions paired by `SecondaryPID`, a Text value stored
/// by `ValueGUID` takes the one whose `ValueList` holds that guid, so a task
/// and a resource definition sharing a `SecondaryPID` each read from their
/// own table.
pub(super) fn read_assignment(
    proj: &Project,
    a: &Assignment,
    kind: CustomKind,
    n: u8,
) -> FieldRead {
    let values = &a.extended_attributes;
    let exact = definitions(proj, kind, n)
        .find_map(|def| Some((def, by_id(values, child(def, "FieldID")?)?)));
    let resolved = exact.or_else(|| {
        let secondary: Vec<_> = definitions(proj, kind, n)
            .filter_map(|def| Some((def, by_id(values, child(def, "SecondaryPID")?)?)))
            .collect();
        let holds_guid = |(def, value): &&(&XmlElement, &ExtendedAttributeValue)| {
            kind == CustomKind::Text
                && value
                    .value_guid
                    .as_deref()
                    .is_some_and(|guid| lookup_value_list(def, guid).is_some())
        };
        secondary
            .iter()
            .find(holds_guid)
            .or(secondary.first())
            .copied()
    });
    match resolved {
        Some((definition, value)) => value_read(proj, definition, value, kind),
        None => unset(proj, kind),
    }
}

/// The definitions whose `FieldName` is this custom field's, in file order.
fn definitions(proj: &Project, kind: CustomKind, n: u8) -> impl Iterator<Item = &XmlElement> {
    let name = Field::Custom(kind, n).name();
    proj.extended_attribute_definitions
        .iter()
        .filter(move |def| child(def, "FieldName").is_some_and(|f| f.eq_ignore_ascii_case(&name)))
}

/// The owner's value stored under this field id.
fn by_id<'a>(values: &'a [ExtendedAttributeValue], id: &str) -> Option<&'a ExtendedAttributeValue> {
    values.iter().find(|value| value.field_id.trim() == id)
}

/// What a field with no resolvable value reads: what Project shows for an
/// unset custom field, with Null underneath (Flag excepted: a false flag
/// reads `No`/`false`, like the built-in flags).
fn unset(proj: &Project, kind: CustomKind) -> FieldRead {
    match kind {
        CustomKind::Text => FieldRead::new("", FieldValue::Null),
        CustomKind::Number => FieldRead::new("0", FieldValue::Null),
        CustomKind::Cost => FieldRead::new(Currency::of(proj).format(0.0), FieldValue::Null),
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
        // `parse` accepts NaN and the inf spellings; only a finite number is
        // a value, anything else keeps its raw text.
        CustomKind::Number => match raw.trim().parse::<f64>().ok().filter(|n| n.is_finite()) {
            Some(number) => FieldRead::new(number.to_string(), FieldValue::Number(number)),
            None => FieldRead::new(raw, FieldValue::Null),
        },
        // MSPDI stores every cost in hundredths, like the built-in costs. A
        // finite but huge value would saturate `Currency::format`'s i128
        // scaled units, so the text could disagree with the value; past 10
        // trillion currency units (1e15 hundredths, below 2^53, so each whole
        // hundredth up to it is exact) read it as unparseable.
        CustomKind::Cost => match raw
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && n.abs() <= 1e15)
        {
            Some(hundredths) => money_value(proj, Some(hundredths)),
            None => FieldRead::new(raw, FieldValue::Null),
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

/// Project's task custom field ids start with these digits (resource fields
/// with 2055, assignment fields with 2558): what tells the task `Text1`
/// definition from the resource one of the same name when no value names
/// it yet.
const TASK_FIELD_ID_PREFIX: &str = "1887";

/// The task definition of a custom field: the first whose `FieldName`
/// matches and whose `FieldID` is a task field's.
fn task_definition(proj: &Project, kind: CustomKind, n: u8) -> Option<&XmlElement> {
    definitions(proj, kind, n)
        .find(|def| child(def, "FieldID").is_some_and(|id| id.starts_with(TASK_FIELD_ID_PREFIX)))
}

/// The `ValueList` entries of a lookup-table definition: `(Value,
/// FieldGUID)`.
fn lookup_entries(definition: &XmlElement) -> Vec<(&str, Option<&str>)> {
    definition
        .children
        .iter()
        .find(|c| c.name == "ValueList")
        .map(|list| {
            list.children
                .iter()
                .filter(|entry| entry.name == "Value")
                .filter_map(|entry| {
                    let value = entry.children.iter().find(|c| c.name == "Value")?;
                    Some((value.text.as_str(), child(entry, "FieldGUID")))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Money text in the plan's currency as units: the symbol, grouping commas
/// and spaces dropped, a negative in parentheses or with a sign.
fn parse_money(proj: &Project, text: &str) -> Option<f64> {
    let currency = Currency::of(proj);
    let mut t = text.trim().to_string();
    if !currency.symbol.is_empty() {
        t = t.replace(&currency.symbol, "");
    }
    let t = t.trim();
    let negative = (t.starts_with('(') && t.ends_with(')')) || t.starts_with('-');
    let digits: String = t
        .chars()
        .filter(|c| !matches!(c, '(' | ')' | '-' | ',' | ' '))
        .collect();
    let units = digits.parse::<f64>().ok().filter(|n| n.is_finite())?;
    Some(if negative { -units } else { units })
}

/// The stored form of `text` typed into task `task`'s custom field `kind`
/// `n`: the field's id and its value (`None` to clear). The field needs a
/// task definition in the plan (Project's Custom Fields dialog makes one);
/// a formula field is computed, not typed; a lookup-table field takes one
/// of its table's values (by its text; the entry's guid is stored with it)
/// unless the table appends new values. Text is kept; a number must parse;
/// a cost is money in the plan's currency, stored in hundredths; a flag is
/// Yes/No, true/false or 1/0; a date is `YYYY-MM-DD` (the task's day start)
/// or `YYYY-MM-DDTHH:MM`; a duration is Project's spelling in working time
/// (`3d`, `2.5 wks`), stored with the unit it was typed in.
pub(super) fn value_for(
    proj: &Project,
    task: &Task,
    kind: CustomKind,
    n: u8,
    text: &str,
) -> Result<(String, Option<ExtendedAttributeValue>), String> {
    let name = Field::Custom(kind, n).name();
    let definition = task_definition(proj, kind, n).ok_or_else(|| {
        format!("{name} has no definition in this plan: define it in Project's Custom Fields first")
    })?;
    let id = child(definition, "FieldID").unwrap_or_default().to_string();
    if child(definition, "Formula").is_some_and(|f| !f.is_empty()) {
        return Err(format!("{name} is computed by a formula"));
    }
    let text = text.trim();
    if text.is_empty() {
        return Ok((id, None));
    }
    let value = |value: String| ExtendedAttributeValue {
        field_id: id.clone(),
        value: Some(value),
        value_guid: None,
        duration_format: None,
    };
    let entries = lookup_entries(definition);
    if !entries.is_empty() {
        if let Some((found, guid)) = entries
            .iter()
            .find(|(v, _)| v.trim().eq_ignore_ascii_case(text))
        {
            let mut stored = value(found.to_string());
            stored.value_guid = guid.map(str::to_string);
            return Ok((id, Some(stored)));
        }
        if child(definition, "AppendNewValues") != Some("1") {
            let listed: Vec<&str> = entries.iter().map(|(v, _)| *v).collect();
            return Err(format!(
                "{name} takes one of its lookup table's values: {}",
                listed.join(", ")
            ));
        }
    }
    let stored = match kind {
        CustomKind::Text => value(text.to_string()),
        CustomKind::Number => {
            let number = text
                .replace(',', "")
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .ok_or_else(|| format!("{name} is a number, not '{text}'"))?;
            value(number.to_string())
        }
        CustomKind::Cost => {
            let units = parse_money(proj, text)
                .ok_or_else(|| format!("{name} is an amount of money, not '{text}'"))?;
            value(((units * 100.0).round()).to_string())
        }
        CustomKind::Flag => {
            let lower = text.to_ascii_lowercase();
            let on = match lower.as_str() {
                "yes" | "true" | "1" => true,
                "no" | "false" | "0" => false,
                _ => return Err(format!("{name} is Yes or No, not '{text}'")),
            };
            value(if on { "1" } else { "0" }.to_string())
        }
        CustomKind::Date | CustomKind::Start | CustomKind::Finish => {
            let at = match DateTime::parse_mspdi(text) {
                Some(dt) if text.contains('T') => dt,
                _ => super::cells::day_start(
                    proj,
                    task,
                    super::cells::parse_cell_date(text)
                        .map_err(|_| format!("{name} is a date (YYYY-MM-DD), not '{text}'"))?,
                ),
            };
            value(at.to_mspdi())
        }
        CustomKind::Duration => {
            let (min, unit) = crate::editor::parse_duration_unit(text, proj).ok_or_else(|| {
                format!("{name} is a duration such as 3d or 2.5 wks, not '{text}'")
            })?;
            let mut stored = value(crate::mspdi::min_to_iso(min));
            stored.duration_format = LagFormat::new(unit, false, false)
                .map(|f| f.code())
                .and_then(|c| u8::try_from(c).ok());
            stored
        }
    };
    Ok((id, Some(stored)))
}
