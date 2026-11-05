//! Font substitution shared by every renderer: the families to try, in order,
//! when a document names one that is not installed (#1900).
//!
//! The screen (the suite's `font_subst`) and PDF export (`pdffont`) both use
//! [`substitutes`], so a missing Office font is replaced by the same family in
//! both. Metric-compatible families come first: Carlito, Caladea and the
//! Liberation fonts have the advance widths of Calibri, Cambria, Arial, Times
//! New Roman and Courier New, so lines break where Word breaks them. Look-alikes
//! follow.

/// The families to try, best first, in place of `family` (any case, surrounding
/// whitespace ignored). Empty for a family with no known substitute.
pub fn substitutes(family: &str) -> &'static [&'static str] {
    match family.trim().to_ascii_lowercase().as_str() {
        "arial" | "arial narrow" => &["Liberation Sans", "Arimo", "Helvetica"],
        "calibri" | "calibri light" => &["Carlito", "Liberation Sans"],
        "cambria" => &["Caladea", "Liberation Serif"],
        "times new roman" | "times" => &["Liberation Serif", "Tinos", "Times", "DejaVu Serif"],
        "courier new" | "courier" | "consolas" => {
            &["Liberation Mono", "Cousine", "Menlo", "DejaVu Sans Mono"]
        }
        "georgia" | "garamond" | "book antiqua" => &["DejaVu Serif", "Liberation Serif"],
        "verdana" | "tahoma" | "trebuchet ms" | "segoe ui" => &["DejaVu Sans", "Liberation Sans"],
        // Aptos is close to Arial's widths; DejaVu Sans (Verdana-wide) breaks
        // lines much earlier, so it comes second.
        "aptos" | "aptos display" | "aptos narrow" => &["Liberation Sans", "DejaVu Sans"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::substitutes;

    #[test]
    fn office_fonts_map_to_their_metric_compatible_families_first() {
        assert_eq!(substitutes("Calibri")[0], "Carlito");
        assert_eq!(substitutes("Cambria")[0], "Caladea");
        assert_eq!(substitutes("Arial")[0], "Liberation Sans");
        assert_eq!(substitutes("Times New Roman")[0], "Liberation Serif");
        assert_eq!(substitutes("Courier New")[0], "Liberation Mono");
    }

    #[test]
    fn aptos_has_its_own_arial_width_substitutes() {
        assert_eq!(substitutes("Aptos"), ["Liberation Sans", "DejaVu Sans"]);
        assert_eq!(substitutes("Aptos Display"), substitutes("Aptos"));
        assert_eq!(substitutes("Aptos Narrow"), substitutes("Aptos"));
        assert_eq!(substitutes("Segoe UI")[0], "DejaVu Sans");
    }

    #[test]
    fn any_case_and_surrounding_whitespace_match() {
        assert_eq!(substitutes("  CALIBRI "), substitutes("Calibri"));
        assert_eq!(
            substitutes("times new roman"),
            substitutes("Times New Roman")
        );
    }

    #[test]
    fn an_unknown_family_has_no_substitutes() {
        assert!(substitutes("Zzz Font").is_empty());
        assert!(substitutes("").is_empty());
    }
}
