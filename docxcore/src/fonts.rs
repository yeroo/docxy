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
        "verdana" | "tahoma" | "trebuchet ms" => &["DejaVu Sans", "Liberation Sans"],
        // Segoe UI sets about as wide as Arial: Selawik is its
        // metric-compatible stand-in, then Arial's widths; Noto Sans and Open
        // Sans set wider, and DejaVu Sans (Verdana-wide) breaks lines much
        // earlier than Word does (#2231).
        "segoe ui" | "segoe ui semibold" | "segoe ui light" | "segoe ui semilight" => &[
            "Selawik",
            "Liberation Sans",
            "Arimo",
            "Noto Sans",
            "Open Sans",
            "DejaVu Sans",
        ],
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
    }

    /// #2231: Segoe UI falls back to a face of its width, not Verdana-wide
    /// DejaVu Sans.
    #[test]
    fn segoe_ui_takes_an_arial_width_face_before_dejavu() {
        let s = substitutes("Segoe UI");
        assert_eq!(&s[..2], ["Selawik", "Liberation Sans"]);
        assert_eq!(s.last(), Some(&"DejaVu Sans"));
        assert_eq!(substitutes("Segoe UI Semibold"), s);
        assert_eq!(substitutes("Segoe UI Light"), s);
        assert_eq!(substitutes("Verdana")[0], "DejaVu Sans");
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
