//! A PDF font's family as the converted document names it (#2257).

/// The family a run set in `base_font` is given: PdfCraft's parse of the
/// name (`pdfcraft_export::font_family`: subset tag, style suffix and
/// `PSMT`/`MT`/`PS` dropped, camel case spaced), then the standard 14 and
/// their common aliases as the fonts Word draws them with, and DejaVu's
/// families as they are installed. Any other family is kept as parsed
/// (`Calibri`); a name that parses to nothing has none, and the run keeps
/// the document's default. Whether the family is
/// installed is the renderer's concern (font substitution, #1900).
pub(crate) fn family(base_font: &str) -> Option<String> {
    let parsed = pdfcraft_export::font_family(base_font).into_iter().next()?;
    let key: String = parsed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let standard = match key.as_str() {
        "helvetica" | "arial" => Some("Arial"),
        "times" | "timesroman" | "timesnewroman" => Some("Times New Roman"),
        "courier" | "couriernew" => Some("Courier New"),
        "symbol" => Some("Symbol"),
        // Not Wingdings: the same codes draw other glyphs there.
        "zapfdingbats" => Some("ZapfDingbats"),
        // Cairo's and LibreOffice's default faces, which the camel-case
        // split would spell `Deja Vu Sans`.
        "dejavusans" => Some("DejaVu Sans"),
        "dejavuserif" => Some("DejaVu Serif"),
        "dejavusansmono" => Some("DejaVu Sans Mono"),
        _ => None,
    };
    Some(standard.map_or(parsed, str::to_string))
}

#[cfg(test)]
mod tests {
    use super::family;

    #[test]
    fn the_standard_fonts_and_their_aliases_map_to_word_s_fonts() {
        for (name, want) in [
            ("Helvetica", "Arial"),
            ("Helvetica-Bold", "Arial"),
            ("Helvetica-BoldOblique", "Arial"),
            ("ArialMT", "Arial"),
            ("Arial-BoldMT", "Arial"),
            ("Arial,Bold", "Arial"),
            ("Times-Roman", "Times New Roman"),
            ("Times-BoldItalic", "Times New Roman"),
            ("TimesNewRomanPSMT", "Times New Roman"),
            ("ABCDEF+TimesNewRomanPS-BoldMT", "Times New Roman"),
            ("Courier", "Courier New"),
            ("Courier-Oblique", "Courier New"),
            ("CourierNewPSMT", "Courier New"),
            ("Symbol", "Symbol"),
            ("ZapfDingbats", "ZapfDingbats"),
        ] {
            assert_eq!(family(name).as_deref(), Some(want), "{name}");
        }
    }

    #[test]
    fn other_families_are_kept_as_installed_and_an_empty_name_has_none() {
        assert_eq!(
            family("IGUEGS+DejaVuSerif").as_deref(),
            Some("DejaVu Serif")
        );
        assert_eq!(
            family("ZXAIRC+DejaVuSans-Bold").as_deref(),
            Some("DejaVu Sans")
        );
        assert_eq!(family("BCDGEE+Calibri").as_deref(), Some("Calibri"));
        assert_eq!(
            family("LiberationSans-Italic").as_deref(),
            Some("Liberation Sans")
        );
        assert_eq!(family("GHIJKL+Calibri-Bold").as_deref(), Some("Calibri"));
        assert_eq!(family(""), None);
        assert_eq!(family("ABCDEF+"), None);
    }
}
