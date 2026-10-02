//! `opccore::codepage`'s tests. They live here, not in the module:
//! `opccore/src/codepage.rs` is written whole by
//! `corpus/tools/gen_rtf_codepages.py`, which would drop them.

use opccore::codepage::{charset_page, decode};

#[test]
fn decodes_ascii_and_the_high_half_of_a_page() {
    assert_eq!(decode(1252, b'A'), Some('A'));
    assert_eq!(decode(1252, 0xe9), Some('\u{e9}'));
    assert_eq!(decode(1252, 0x80), Some('\u{20ac}'));
    assert_eq!(decode(1251, 0xc6), Some('\u{416}'));
    // Windows maps 1252's unassigned 0x81 to U+0081, as RichEdit does.
    assert_eq!(decode(1252, 0x81), Some('\u{81}'));
    // 932 is a DBCS page with no table here: only ASCII decodes.
    assert_eq!(decode(932, 0x82), None);
    assert_eq!(decode(932, b'a'), Some('a'));
}

#[test]
fn charset_names_its_page() {
    assert_eq!(charset_page(204, 1252), Some(1251));
    assert_eq!(charset_page(1, 1250), Some(1250));
    assert_eq!(charset_page(2, 1252), Some(42));
    assert_eq!(charset_page(128, 1252), None);
}
