//! The Home ribbon's Font and Font Size boxes, the sheet's (#1344) and the
//! document's (#1361): what they show and what Enter makes of the typed text.

/// The font a cell without one of its own has, and its size.
pub(crate) const DEFAULT_FONT: &str = "Calibri";
pub(crate) const DEFAULT_SIZE: f64 = 11.0;

/// Excel's bounds for a font size, in points.
pub(crate) const MIN_SIZE: f64 = 1.0;
pub(crate) const MAX_SIZE: f64 = 409.0;

/// Excel's longest font name, in characters.
pub(crate) const MAX_NAME: usize = 31;

/// The font name typed in the box, trimmed and cut to Excel's limit.
pub(crate) fn clamp_name(text: &str) -> String {
    text.trim().chars().take(MAX_NAME).collect()
}

/// A size as the box shows it: `11`, `10.5`.
pub(crate) fn size_text(size: f64) -> String {
    if size.fract() == 0.0 {
        format!("{}", size as i64)
    } else {
        format!("{size}")
    }
}

/// The size typed in the box: a finite number of points within Excel's
/// bounds (a comma is a decimal mark too).
pub(crate) fn parse_size(text: &str) -> Option<f64> {
    let n: f64 = text.trim().replace(',', ".").parse().ok()?;
    (n.is_finite() && (MIN_SIZE..=MAX_SIZE).contains(&n)).then_some(n)
}

/// Word's bounds for a document font size, in points.
pub(crate) const MAX_DOC_SIZE: f64 = 1638.0;

/// The size typed in a document's box, in half points (what the editor
/// stores): a number from 1 to 1638 points, rounded to the nearest half
/// point; a comma is a decimal mark too.
pub(crate) fn parse_doc_size(text: &str) -> Option<u32> {
    let n: f64 = text.trim().replace(',', ".").parse().ok()?;
    if !n.is_finite() || !(MIN_SIZE..=MAX_DOC_SIZE).contains(&n) {
        return None;
    }
    Some((n * 2.0).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_font_name_is_cut_to_excels_limit() {
        assert_eq!(clamp_name("  Arial "), "Arial");
        let long = "x".repeat(40);
        assert_eq!(clamp_name(&long).chars().count(), 31);
        assert_eq!(clamp_name(&"é".repeat(40)).chars().count(), 31);
    }

    #[test]
    fn a_size_shows_without_a_trailing_zero() {
        assert_eq!(size_text(11.0), "11");
        assert_eq!(size_text(10.5), "10.5");
        assert_eq!(size_text(409.0), "409");
    }

    #[test]
    fn a_typed_size_is_a_number_in_excels_range() {
        assert_eq!(parse_size("11"), Some(11.0));
        assert_eq!(parse_size(" 10.5 "), Some(10.5));
        assert_eq!(parse_size("10,5"), Some(10.5));
        assert_eq!(parse_size("1"), Some(1.0));
        assert_eq!(parse_size("409"), Some(409.0));
        for bad in ["0", "0.5", "410", "-3", "abc", "NaN", "inf", "", " "] {
            assert_eq!(parse_size(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_typed_document_size_is_half_points_in_words_range() {
        assert_eq!(parse_doc_size("11"), Some(22));
        assert_eq!(parse_doc_size(" 10.5 "), Some(21));
        assert_eq!(parse_doc_size("10,5"), Some(21));
        assert_eq!(parse_doc_size("1"), Some(2));
        assert_eq!(parse_doc_size("1638"), Some(3276));
        // Rounded to the nearest half point.
        assert_eq!(parse_doc_size("10.25"), Some(21));
        assert_eq!(parse_doc_size("10.24"), Some(20));
        for bad in ["0", "0.9", "1639", "-3", "abc", "NaN", "inf", "", " "] {
            assert_eq!(parse_doc_size(bad), None, "{bad:?}");
        }
    }
}
