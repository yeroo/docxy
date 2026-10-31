//! Excel's check on a Save As target (#804, FIL-CASE-031), shared by the
//! suite and terminal `xlsxy`. (xlcomshim's `Workbook.SaveAs` does not call
//! it.)
//!
//! Excel refuses a file name holding `< > ? [ ] : | * "` (the brackets are
//! external-reference syntax) or a full path over 218 UTF-16 units. In-place
//! Save never asks, so a workbook opened from such a name still saves over
//! itself; only Save As runs this check.

use std::path::{Component, Path};

/// Excel's longest workbook path, in UTF-16 units as Windows counts.
pub const MAX_PATH: usize = 218;

/// What Excel's Save As says of a name it refuses.
pub const BAD_NAME: &str = "That file name is not valid: a workbook name cannot contain < > ? [ ] : | \" or *, and its full path cannot be longer than 218 characters";

/// The characters a workbook file name cannot hold.
const BAD_CHARS: [char; 9] = ['<', '>', '?', '[', ']', ':', '|', '*', '"'];

/// Check `path`, the file that will be written (so an added extension
/// counts), taken as given: neither canonicalised nor resolved against the
/// current folder. Only the file name is checked for characters: a folder
/// named `[old]` is fine.
pub fn check(path: &Path) -> Result<(), &'static str> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let bad = has_bad_char(&name);
    let long = path.as_os_str().to_string_lossy().encode_utf16().count() > MAX_PATH;
    if bad || long { Err(BAD_NAME) } else { Ok(()) }
}

/// Whether a name holds a character a workbook name cannot.
pub fn has_bad_char(name: &str) -> bool {
    name.contains(BAD_CHARS)
}

/// Whether a name typed in a field that is joined to a folder (Backstage
/// Save As) passes what the join would hide from [`check`] (#1807): its last
/// piece (after the last `/` or `\`, split on both on every OS: on Unix a
/// `\` stays in the file name, which [`check`] still sees) must hold no bad
/// character, and it must not be drive-relative. On Windows `c:report` is
/// drive `C:` plus `report`, and joining it replaces the folder. A folder part
/// such as `[old]/book` is fine; the joined path still goes through [`check`].
pub fn typed_name_ok(name: &str) -> bool {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let path = Path::new(name);
    let drive_relative =
        matches!(path.components().next(), Some(Component::Prefix(_))) && !path.has_root();
    !has_bad_char(last) && !drive_relative
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn every_bad_character_in_the_name_is_refused() {
        for c in BAD_CHARS {
            let p = PathBuf::from(format!("dir/a{c}b.xlsx"));
            assert_eq!(check(&p), Err(BAD_NAME), "{c}");
        }
        assert_eq!(check(Path::new("dir/a b (1).xlsx")), Ok(()));
        assert_eq!(check(Path::new("dir/a'b#c.xlsx")), Ok(()));
    }

    #[test]
    fn a_typed_name_checks_its_last_piece() {
        assert!(!typed_name_ok("c:report.xlsx"));
        for c in BAD_CHARS {
            assert!(!typed_name_ok(&format!("ab{c}c.xlsx")), "{c}");
            assert!(!typed_name_ok(&format!("sub/ab{c}c.xlsx")), "{c}");
            assert!(!typed_name_ok(&format!("sub\\ab{c}c.xlsx")), "{c}");
        }
        assert!(typed_name_ok("report (1).xlsx"));
        assert!(typed_name_ok("sub/report.xlsx"));
        assert!(typed_name_ok("[old]/book.xlsx"));
        assert!(typed_name_ok("[old]\\book.xlsx"));
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_relative_typed_name_is_refused_on_windows() {
        assert!(!typed_name_ok("c:sub\\r.xlsx"));
        assert!(!typed_name_ok("c:sub/r.xlsx"));
        assert!(typed_name_ok("C:\\Users\\me\\report.xlsx"));
    }

    #[test]
    fn a_folder_may_hold_the_characters() {
        assert_eq!(check(Path::new("dir/[old]/book.xlsx")), Ok(()));
        assert_eq!(check(Path::new("[a]?/b*/book.xlsx")), Ok(()));
    }

    #[test]
    fn the_path_counts_up_to_218_utf16_units() {
        let at = format!("d/{}", "x".repeat(MAX_PATH - 2));
        assert_eq!(at.encode_utf16().count(), MAX_PATH);
        assert_eq!(check(Path::new(&at)), Ok(()));
        let over = format!("d/{}", "x".repeat(MAX_PATH - 1));
        assert_eq!(check(Path::new(&over)), Err(BAD_NAME));
    }

    #[test]
    fn a_wide_character_counts_in_utf16_units() {
        // é is one unit, as is an ASCII letter: 218 still fits.
        let bmp = format!("d/{}é", "x".repeat(MAX_PATH - 3));
        assert_eq!(bmp.encode_utf16().count(), MAX_PATH);
        assert_eq!(check(Path::new(&bmp)), Ok(()));
        // An emoji is two units (a surrogate pair), though one char: 218
        // chars are 219 units.
        let wide = format!("d/{}\u{1F600}", "x".repeat(MAX_PATH - 3));
        assert_eq!(wide.chars().count(), MAX_PATH);
        assert_eq!(wide.encode_utf16().count(), MAX_PATH + 1);
        assert_eq!(check(Path::new(&wide)), Err(BAD_NAME));
    }

    #[test]
    fn an_empty_path_is_fine() {
        assert_eq!(check(Path::new("")), Ok(()));
    }
}
