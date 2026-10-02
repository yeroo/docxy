//! Excel's rules for names: defined names and table names share them.

/// Whether `name` is a name Excel accepts: 1–255 characters, starting with a
/// letter, `_` or `\`, continuing with letters, digits, `_` and `.`, and not
/// readable as a cell reference in either A1 (`AB12`, `XFD1048576`) or R1C1
/// (`R1C1`, `R`, `C3`, `rc`) notation. Uniqueness is the caller's to check.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("A name can't be empty".into());
    }
    if name.chars().count() > 255 {
        return Err("A name can't be longer than 255 characters".into());
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap_or(' ');
    if !(first.is_alphabetic() || first == '_' || first == '\\') {
        return Err(format!(
            "\"{name}\" must start with a letter, an underscore or a backslash"
        ));
    }
    if let Some(bad) = chars.find(|c| !(c.is_alphanumeric() || *c == '_' || *c == '.')) {
        return Err(format!("\"{name}\" can't contain '{bad}'"));
    }
    if looks_like_a1(name) || looks_like_r1c1(name) {
        return Err(format!("\"{name}\" looks like a cell reference"));
    }
    Ok(())
}

/// `AB12`: one to three letters naming a column up to XFD, then a row in
/// 1..=1048576.
fn looks_like_a1(name: &str) -> bool {
    let letters = name.chars().take_while(|c| c.is_ascii_alphabetic()).count();
    let digits = &name[letters..];
    if !(1..=3).contains(&letters)
        || digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return false;
    }
    let col = name[..letters].bytes().fold(0u32, |n, b| {
        n * 26 + u32::from(b.to_ascii_uppercase() - b'A' + 1)
    });
    let row = digits.parse::<u64>().unwrap_or(0);
    col <= 16_384 && (1..=1_048_576).contains(&row)
}

/// `R`, `C`, `RC`, `R2`, `C3`, `R1C1` (either case).
fn looks_like_r1c1(name: &str) -> bool {
    let b = name.as_bytes();
    let mut i = 0;
    let mut part = |letter: u8| {
        if b.get(i).is_some_and(|c| c.eq_ignore_ascii_case(&letter)) {
            i += 1;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            true
        } else {
            false
        }
    };
    let r = part(b'r');
    let c = part(b'c');
    (r || c) && i == b.len()
}

#[cfg(test)]
mod tests {
    use super::check_name;

    #[test]
    fn excel_name_rules() {
        for ok in [
            "Sales",
            "_t",
            "\\x",
            "Sales.2024",
            "Revenue_Q1",
            "AB",
            "XFE1",
            "Données",
        ] {
            assert!(check_name(ok).is_ok(), "{ok} should be accepted");
        }
        let long = "a".repeat(256);
        for bad in [
            "",
            "1Sales",
            ".x",
            "Sales Q1",
            "A-B",
            "A1",
            "ab12",
            "XFD1048576",
            "R1C1",
            "R2",
            "C3",
            "RC",
            "R",
            "C",
            "r",
            "c",
            &long,
        ] {
            assert!(check_name(bad).is_err(), "{bad} should be refused");
        }
        assert!(check_name(&"a".repeat(255)).is_ok());
    }
}
