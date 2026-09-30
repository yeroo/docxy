//! Auto-filter criteria: parse a typed comparison and evaluate it against a
//! cell value. Used to hide non-matching rows in a filtered region.

use opccore::xml::{Event, XmlParser};

use crate::sheet::{CellValue, Sheet, Styles};

/// Parse a filter criteria into `(operator, operand)`: ">500", "<=100", "<>X",
/// "=Laptop", or a plain value. Unlike the CF parser, the default operator is
/// `equal` (picking a value is the common filter case).
pub fn parse(s: &str) -> Option<(&'static str, String)> {
    let s = s.trim();
    let (op, rest) = if let Some(r) = s.strip_prefix(">=") {
        ("greaterThanOrEqual", r)
    } else if let Some(r) = s.strip_prefix("<=") {
        ("lessThanOrEqual", r)
    } else if let Some(r) = s.strip_prefix("<>") {
        ("notEqual", r)
    } else if let Some(r) = s.strip_prefix('>') {
        ("greaterThan", r)
    } else if let Some(r) = s.strip_prefix('<') {
        ("lessThan", r)
    } else if let Some(r) = s.strip_prefix('=') {
        ("equal", r)
    } else {
        ("equal", s)
    };
    let rest = rest.trim();
    if rest.is_empty() {
        None
    } else {
        Some((op, rest.to_string()))
    }
}

/// Whether a cell value satisfies `(op, operand)`. Numbers compare numerically
/// (when the operand parses as a number); text compares case-insensitively
/// (equality) or lexicographically (ordering). Blank cells satisfy only
/// `notEqual`.
pub fn matches(value: Option<&CellValue>, op: &str, operand: &str) -> bool {
    match value {
        Some(CellValue::Number(n)) => match operand.parse::<f64>() {
            Ok(o) => match op {
                "greaterThan" => *n > o,
                "greaterThanOrEqual" => *n >= o,
                "lessThan" => *n < o,
                "lessThanOrEqual" => *n <= o,
                "equal" => *n == o,
                "notEqual" => *n != o,
                _ => false,
            },
            // A number cell can't equal a non-numeric operand.
            Err(_) => op == "notEqual",
        },
        Some(CellValue::Text(t)) => {
            let (a, b) = (t.trim().to_lowercase(), operand.trim().to_lowercase());
            match op {
                "equal" => a == b,
                "notEqual" => a != b,
                "greaterThan" => a > b,
                "greaterThanOrEqual" => a >= b,
                "lessThan" => a < b,
                "lessThanOrEqual" => a <= b,
                _ => false,
            }
        }
        Some(CellValue::Bool(v)) => {
            let s = if *v { "true" } else { "false" };
            match op {
                "equal" => s.eq_ignore_ascii_case(operand.trim()),
                "notEqual" => !s.eq_ignore_ascii_case(operand.trim()),
                _ => false,
            }
        }
        _ => op == "notEqual", // blank / empty
    }
}

/// A saved `<autoFilter>` (a worksheet's or a table's): its range and the
/// criteria of each filtered column.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutoFilter {
    /// (r1, c1, r2, c2), 0-based, header row included.
    pub range: (u32, u32, u32, u32),
    /// (column offset from `range`'s left, criteria).
    pub columns: Vec<(u32, ColumnFilter)>,
}

/// One `<filterColumn>`'s criteria.
#[derive(Clone, Debug, PartialEq)]
pub enum ColumnFilter {
    /// `<filters>`: the shown values (compared with the cell's displayed
    /// text), and whether blanks are shown.
    Values { vals: Vec<String>, blank: bool },
    /// `<customFilters>`: one or two `(operator, value)` conditions, AND'd
    /// when `and`, else OR'd.
    Custom {
        and: bool,
        conds: Vec<(String, String)>,
    },
    /// Anything we cannot re-evaluate (`top10`, `dynamicFilter`, date groups,
    /// colour and icon filters).
    Unsupported,
}

/// The top-level `<autoFilter>` of a worksheet or table part (not one inside
/// a custom sheet view), if it has a range.
pub fn parse_auto_filter(xml: &str) -> Option<AutoFilter> {
    let mut p = XmlParser::new(xml);
    let mut depth = 0usize;
    let mut af: Option<AutoFilter> = None;
    // Depth of the autoFilter element while inside it.
    let mut in_af: Option<usize> = None;
    let mut col: Option<(u32, Option<ColumnFilter>)> = None;
    loop {
        match p.next() {
            Event::Start => {
                depth += 1;
                let name = p.name().rsplit(':').next().unwrap_or("");
                if in_af.is_none() {
                    if depth == 2 && name == "autoFilter" {
                        let range = crate::sheet::parse_range_name(p.attr("ref"))?;
                        af = Some(AutoFilter {
                            range,
                            columns: Vec::new(),
                        });
                        in_af = Some(depth);
                    }
                    continue;
                }
                match name {
                    "filterColumn" => {
                        col = Some((p.attr("colId").parse().unwrap_or(0), None));
                    }
                    "filters" => {
                        if let Some((_, f)) = col.as_mut() {
                            *f = Some(ColumnFilter::Values {
                                vals: Vec::new(),
                                blank: matches!(p.attr("blank"), "1" | "true"),
                            });
                        }
                    }
                    "filter" => {
                        if let Some((_, Some(ColumnFilter::Values { vals, .. }))) = col.as_mut() {
                            let mut v = String::new();
                            XmlParser::append_decoded(p.attr("val"), &mut v);
                            vals.push(v);
                        }
                    }
                    "customFilters" => {
                        if let Some((_, f)) = col.as_mut() {
                            *f = Some(ColumnFilter::Custom {
                                and: matches!(p.attr("and"), "1" | "true"),
                                conds: Vec::new(),
                            });
                        }
                    }
                    "customFilter" => {
                        if let Some((_, Some(ColumnFilter::Custom { conds, .. }))) = col.as_mut() {
                            let op = match p.attr("operator") {
                                "" => "equal".to_string(),
                                o => o.to_string(),
                            };
                            let mut v = String::new();
                            XmlParser::append_decoded(p.attr("val"), &mut v);
                            conds.push((op, v));
                        }
                    }
                    "dateGroupItem" | "top10" | "dynamicFilter" | "colorFilter" | "iconFilter" => {
                        if let Some((_, f)) = col.as_mut() {
                            *f = Some(ColumnFilter::Unsupported);
                        }
                    }
                    _ => {}
                }
            }
            Event::End => {
                let name = p.name().rsplit(':').next().unwrap_or("");
                if in_af.is_some() && name == "filterColumn" {
                    if let (Some((id, f)), Some(af)) = (col.take(), af.as_mut()) {
                        af.columns
                            .push((id, f.unwrap_or(ColumnFilter::Unsupported)));
                    }
                }
                if in_af == Some(depth) {
                    return af;
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => return af,
            Event::Text => {}
        }
    }
}

/// The rows an applied auto-filter hides: rows below its header that are
/// hidden *and* fail its criteria. A hidden row that passes them was hidden
/// by hand. When a column's criteria cannot be re-evaluated, every hidden
/// row in the range counts as filtered.
pub fn filtered_rows(sheet: &Sheet, styles: &Styles, date1904: bool, af: &AutoFilter) -> Vec<u32> {
    let (r1, c1, r2, _) = af.range;
    let unsupported = af
        .columns
        .iter()
        .any(|(_, f)| matches!(f, ColumnFilter::Unsupported));
    ((r1 + 1)..=r2)
        .filter(|&r| sheet.row_hidden(r))
        .filter(|&r| {
            unsupported
                || !af.columns.iter().all(|(off, f)| {
                    let cell = sheet.cell(r, c1 + off);
                    column_passes(f, cell.map(|c| &c.value), || {
                        cell.map(|c| {
                            crate::sheet::format_with(&styles.xf(c.style), &c.value, date1904)
                        })
                        .unwrap_or_default()
                    })
                })
        })
        .collect()
}

/// Whether a cell passes one column's criteria. `shown` gives its displayed
/// text, which `<filter val>` compares against.
fn column_passes(f: &ColumnFilter, value: Option<&CellValue>, shown: impl Fn() -> String) -> bool {
    let blank = matches!(value, None | Some(CellValue::Empty))
        || matches!(value, Some(CellValue::Text(t)) if t.is_empty());
    match f {
        ColumnFilter::Values { vals, blank: b } => {
            if blank {
                return *b;
            }
            let text = shown();
            vals.iter()
                .any(|v| v.trim().eq_ignore_ascii_case(text.trim()))
        }
        ColumnFilter::Custom { and, conds } => {
            let hit = |(op, val): &(String, String)| match (op.as_str(), value) {
                ("equal" | "notEqual", Some(CellValue::Text(t))) if val.contains(['*', '?']) => {
                    crate::formula::wildcard_match(val, t) == (op == "equal")
                }
                _ => matches(value, op, val),
            };
            if *and {
                conds.iter().all(hit)
            } else {
                conds.iter().any(hit)
            }
        }
        ColumnFilter::Unsupported => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::CellValue;

    #[test]
    fn parse_defaults_to_equal() {
        assert_eq!(parse("Laptop"), Some(("equal", "Laptop".into())));
        assert_eq!(parse(">500"), Some(("greaterThan", "500".into())));
        assert_eq!(parse("<>0"), Some(("notEqual", "0".into())));
        assert_eq!(parse("  "), None);
    }

    #[test]
    fn matches_numbers_and_text() {
        let n = CellValue::Number(700.0);
        assert!(matches(Some(&n), "greaterThan", "500"));
        assert!(!matches(Some(&n), "lessThan", "500"));
        let t = CellValue::Text("Laptop".into());
        assert!(matches(Some(&t), "equal", "laptop"));
        assert!(!matches(Some(&t), "equal", "Dock"));
        assert!(matches(None, "notEqual", "x"));
        assert!(!matches(None, "equal", "x"));
    }
}
