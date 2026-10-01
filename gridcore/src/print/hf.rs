//! Header and footer strings: the editor's `&[…]` form, Excel's stored codes,
//! the three sections, and the printed text.
//!
//! Excel stores one string per header or footer (`&LLeft&CPage &P of &N`):
//! `&L`, `&C` and `&R` start the left, centre and right sections, `&&` is a
//! literal ampersand, and the rest of the `&` codes are fields (`&P` page,
//! `&N` pages, `&D` date, `&T` time, `&Z` path, `&F` file, `&A` sheet, `&G`
//! picture) or formatting (`&B`, `&"Arial,Bold"`, `&12`, `&KFF0000`, …). The
//! header/footer editor shows fields as `&[Page]` and so on; the codec maps
//! between the two and leaves formatting codes alone.

/// The longest section Excel accepts, codes included (FIL-139).
pub const MAX_SECTION: usize = 255;

/// The editor's field tokens and the code each one is stored as.
const FIELDS: [(&str, char); 8] = [
    ("&[Page]", 'P'),
    ("&[Pages]", 'N'),
    ("&[Date]", 'D'),
    ("&[Time]", 'T'),
    ("&[Path]", 'Z'),
    ("&[File]", 'F'),
    ("&[Tab]", 'A'),
    ("&[Picture]", 'G'),
];

/// One section in the editor's form: field tokens become their codes, `&&`
/// stays a literal ampersand, and every other `&` code passes through.
pub fn encode(editor: &str) -> String {
    let mut out = String::with_capacity(editor.len());
    let mut rest = editor;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if rest.starts_with("&&") {
            out.push_str("&&");
            rest = &rest[2..];
        } else if let Some((tok, code)) = FIELDS.iter().find(|(t, _)| rest.starts_with(t)) {
            out.push('&');
            out.push(*code);
            rest = &rest[tok.len()..];
        } else if rest.len() == 1 {
            // A lone `&` ending the section would join the next section's
            // `&C`/`&R` into `&&`: write it as a literal ampersand.
            out.push_str("&&");
            rest = "";
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// One stored section in the editor's form: the inverse of [`encode`].
pub fn decode(stored: &str) -> String {
    let mut out = String::with_capacity(stored.len());
    let mut chars = stored.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('&') => {
                chars.next();
                out.push_str("&&");
            }
            Some(code) => match FIELDS.iter().find(|(_, k)| *k == code) {
                Some((tok, _)) => {
                    chars.next();
                    out.push_str(tok);
                }
                None => out.push('&'),
            },
            None => out.push('&'),
        }
    }
    out
}

/// A header or footer split into its three sections, each in stored form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sections {
    pub left: String,
    pub center: String,
    pub right: String,
}

impl Sections {
    /// Split a stored string at its `&L` / `&C` / `&R` codes. Text before the
    /// first one prints centred, as Excel prints it; a section named twice
    /// collects both runs.
    pub fn parse(stored: &str) -> Sections {
        let mut s = Sections::default();
        let mut cur = 'C';
        let mut run = String::new();
        let mut chars = stored.chars().peekable();
        let flush = |s: &mut Sections, cur: char, run: &mut String| {
            let target = match cur {
                'L' => &mut s.left,
                'R' => &mut s.right,
                _ => &mut s.center,
            };
            target.push_str(run);
            run.clear();
        };
        while let Some(c) = chars.next() {
            if c != '&' {
                run.push(c);
                continue;
            }
            match chars.peek().copied() {
                Some(k @ ('L' | 'C' | 'R')) => {
                    chars.next();
                    flush(&mut s, cur, &mut run);
                    cur = k;
                }
                Some(k) => {
                    // `&&` and every other code stay as they are.
                    chars.next();
                    run.push('&');
                    run.push(k);
                }
                None => run.push('&'),
            }
        }
        flush(&mut s, cur, &mut run);
        s
    }

    /// The stored string: `&L…&C…&R…`, empty sections left out.
    pub fn compose(&self) -> String {
        let mut out = String::new();
        for (code, text) in [('L', &self.left), ('C', &self.center), ('R', &self.right)] {
            if !text.is_empty() {
                out.push('&');
                out.push(code);
                out.push_str(text);
            }
        }
        out
    }

    /// The sections from the editor's form, each checked against
    /// [`MAX_SECTION`]. A section code (`&L`, `&C`, `&R`) inside a section is
    /// refused: it would start another section, and in the editor a literal
    /// ampersand is typed `&&` (FIL-136).
    pub fn from_editor(left: &str, center: &str, right: &str) -> Result<Sections, String> {
        for (name, text) in [("left", left), ("center", center), ("right", right)] {
            if let Some(k) = ['L', 'C', 'R'].into_iter().find(|&k| has_code(text, k)) {
                return Err(format!(
                    "the {name} section has \"&{k}\", which starts a section; type a literal ampersand as \"&&\""
                ));
            }
        }
        let s = Sections {
            left: encode(left),
            center: encode(center),
            right: encode(right),
        };
        for (name, text) in [
            ("left", &s.left),
            ("center", &s.center),
            ("right", &s.right),
        ] {
            let n = text.chars().count();
            if n > MAX_SECTION {
                return Err(format!(
                    "the {name} section is {n} characters; a section holds at most {MAX_SECTION}"
                ));
            }
        }
        Ok(s)
    }
}

/// Does a stored string use field code `code` (not inside `&&`)?
pub fn has_code(stored: &str, code: char) -> bool {
    let mut chars = stored.chars();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.next() {
                Some(k) if k == code => return true,
                _ => {}
            }
        }
    }
    false
}

/// A stored string without its picture codes (`&G`), for page setup copied
/// to a sheet that has no header picture of its own.
pub fn strip_pictures(stored: &str) -> String {
    let mut out = String::with_capacity(stored.len());
    let mut chars = stored.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('G') => {}
            Some(k) => {
                out.push('&');
                out.push(k);
            }
            None => out.push('&'),
        }
    }
    out
}

/// What the field codes print as on one page.
#[derive(Clone, Debug, Default)]
pub struct Fields {
    pub page: u32,
    pub pages: u32,
    pub date: String,
    pub time: String,
    /// The workbook's folder, with a trailing separator (`&Z`).
    pub path: String,
    /// The workbook's file name (`&F`).
    pub file: String,
    /// The sheet's name (`&A`).
    pub tab: String,
}

/// One stored section as it prints: fields filled in, `&&` as `&`,
/// formatting codes and pictures dropped. `&P+n` / `&P-n` offset the page
/// number, as Excel does.
pub fn render(section: &str, f: &Fields) -> String {
    let mut out = String::new();
    let chars: Vec<char> = section.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '&' {
            out.push(c);
            continue;
        }
        let Some(&k) = chars.get(i) else {
            break;
        };
        i += 1;
        match k {
            '&' => out.push('&'),
            'P' => {
                let mut n = i64::from(f.page);
                // `&P+3`, `&P-1`.
                if let Some(&sign @ ('+' | '-')) = chars.get(i) {
                    let digits: String = chars[i + 1..]
                        .iter()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(d) = digits.parse::<i64>() {
                        n = if sign == '+' {
                            n.saturating_add(d)
                        } else {
                            n.saturating_sub(d)
                        };
                        i += 1 + digits.len();
                    }
                }
                out.push_str(&n.to_string());
            }
            'N' => out.push_str(&f.pages.to_string()),
            'D' => out.push_str(&f.date),
            'T' => out.push_str(&f.time),
            'Z' => out.push_str(&f.path),
            'F' => out.push_str(&f.file),
            'A' => out.push_str(&f.tab),
            // `&"Font,Style"`.
            '"' => {
                while i < chars.len() && chars[i] != '"' {
                    i += 1;
                }
                i += 1;
            }
            // `&KRRGGBB` or a theme colour `&KTTSNNN`: six characters.
            'K' => i = (i + 6).min(chars.len()),
            // A font size: `&12`.
            d if d.is_ascii_digit() => {
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            // B I U E S X Y G and anything unknown print nothing.
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_fields_are_stored_as_excel_codes() {
        // FIL-CASE-041.
        let s = Sections::from_editor("&[Path]&[File]", "", "").unwrap();
        assert_eq!(s.compose(), "&L&Z&F");
        let s = Sections::from_editor("", "R&&D &[Page] of &[Pages]", "&[Tab]").unwrap();
        assert_eq!(s.compose(), "&CR&&D &P of &N&R&A");
        assert_eq!(encode("&[Date] &[Time] &[Picture]"), "&D &T &G");
    }

    #[test]
    fn formatting_codes_and_double_ampersands_pass_through() {
        let editor = r#"&"Arial,Bold"&12R&&D &[Page]&KFF0000"#;
        let stored = encode(editor);
        assert_eq!(stored, r#"&"Arial,Bold"&12R&&D &P&KFF0000"#);
        assert_eq!(decode(&stored), editor);
    }

    #[test]
    fn a_stored_string_splits_into_its_sections_and_back() {
        let stored = "&LLeft &P&CR&&D&RRight";
        let s = Sections::parse(stored);
        assert_eq!(s.left, "Left &P");
        assert_eq!(s.center, "R&&D");
        assert_eq!(s.right, "Right");
        assert_eq!(s.compose(), stored);
        // No section code: centred.
        assert_eq!(Sections::parse("Page &P").center, "Page &P");
        // `&&L` is a literal "&L", not the left section.
        assert_eq!(Sections::parse("A&&LB").center, "A&&LB");
    }

    #[test]
    fn a_section_code_inside_a_section_is_refused() {
        // FIX r2 m4: in the editor a literal ampersand is `&&`.
        let e = Sections::from_editor("Smith &Co", "", "").unwrap_err();
        assert!(e.contains("left section") && e.contains("&C"), "{e}");
        assert!(Sections::from_editor("", "", "&[Picture]&R&[Picture]").is_err());
        assert!(Sections::from_editor("&[Picture]&R&[Picture]", "", "").is_err());
        let s = Sections::from_editor("Smith &&Co", "&B&[Page]", "").unwrap();
        assert_eq!(Sections::parse(&s.compose()).left, "Smith &&Co");
    }

    #[test]
    fn a_trailing_ampersand_stays_in_its_section() {
        // FIX r1 m5.
        let s = Sections::from_editor("Smith &", "Page &[Page]", "").unwrap();
        assert_eq!(s.compose(), "&LSmith &&&CPage &P");
        let back = Sections::parse(&s.compose());
        assert_eq!(back.left, "Smith &&");
        assert_eq!(back.center, "Page &P");
        assert_eq!(decode(&back.left), "Smith &&");
    }

    #[test]
    fn a_section_over_255_characters_is_refused() {
        let long = "x".repeat(256);
        assert!(Sections::from_editor(&long, "", "").is_err());
        assert!(Sections::from_editor(&"x".repeat(255), "", "").is_ok());
        // Measured in stored form: `&[Page]` is two characters.
        let fields = format!("{}&[Page]", "x".repeat(253));
        assert!(Sections::from_editor("", &fields, "").is_ok());
    }

    #[test]
    fn rendering_fills_fields_and_drops_formatting() {
        let f = Fields {
            page: 2,
            pages: 5,
            date: "1/2/2026".into(),
            time: "9:30".into(),
            path: "C:\\work\\".into(),
            file: "Book.xlsx".into(),
            tab: "Sheet1".into(),
        };
        assert_eq!(render("Page &P of &N", &f), "Page 2 of 5");
        assert_eq!(render(r#"&"Arial,Bold"&14R&&D&B"#, &f), "R&D");
        assert_eq!(
            render("&Z&F &A &D &T", &f),
            "C:\\work\\Book.xlsx Sheet1 1/2/2026 9:30"
        );
        assert_eq!(render("&KFF0000red&G", &f), "red");
        assert_eq!(render("&P+10", &f), "12");
        // FIX r2 m2: an absurd offset saturates instead of overflowing.
        assert_eq!(render("&P+9223372036854775807", &f), i64::MAX.to_string());
        assert_eq!(
            render("&P-9223372036854775807", &f),
            (2 - i64::MAX).to_string()
        );
    }

    #[test]
    fn pictures_are_found_and_stripped() {
        assert!(has_code(&Sections::parse("&C&G").center, 'G'));
        assert!(!has_code(&Sections::parse("&C&&G").center, 'G'));
        assert_eq!(strip_pictures("&L&G&CTitle&&G"), "&L&CTitle&&G");
    }
}
