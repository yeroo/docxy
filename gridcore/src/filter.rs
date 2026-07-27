//! Auto-filter criteria: parse a typed comparison and evaluate it against a
//! cell value. Used to hide non-matching rows in a filtered region.

use crate::sheet::CellValue;

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
