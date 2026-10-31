//! Excel's check on a Save As target (#804, FIL-CASE-031), shared by every
//! host that saves a workbook: the suite and terminal `xlsxy`.
//!
//! Excel refuses a file name holding `< > ? [ ] : | * "` (the brackets are
//! external-reference syntax) or a full path over 218 UTF-16 units. In-place
//! Save never asks, so a workbook opened from such a name still saves over
//! itself; only Save As runs this check.

use std::path::Path;

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
    let bad = name.contains(BAD_CHARS);
    let long = path.as_os_str().to_string_lossy().encode_utf16().count() > MAX_PATH;
    if bad || long { Err(BAD_NAME) } else { Ok(()) }
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
