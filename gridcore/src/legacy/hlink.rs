//! The BIFF8 HLINK record (MS-XLS 2.4.140) and the Hyperlink Object inside
//! it (MS-OSHARED 2.3.7.1), both ways (#1726): [`body`] for the `.xls`
//! writer, [`parse`] for the reader. A model target (`Sheet::hyperlinks`) is
//! written as Excel writes it:
//!
//! - `#loc`: a location string alone;
//! - a UNC path (`\\server\share\...`): the path as a string moniker
//!   (`hlstmfMonikerSavedAsStr`);
//! - a target with a URI scheme (`https:`, `mailto:`, `file:`): a URL
//!   moniker;
//! - any other path: a File moniker, absolute for a drive or `\`-rooted path,
//!   else relative, its leading `..\` segments counted in `cAnti`.
//!
//! No display name, frame or ScreenTip: the model holds none.

use super::Le;
use super::ptg::utf16;
use crate::xlsx::{has_scheme, model_target};

/// A cell rectangle: (r1, c1, r2, c2), 0-based and inclusive.
pub(crate) type Rect = (u32, u32, u32, u32);

/// The HLINK record id.
pub(crate) const HLINK: u16 = 0x01B8;
/// HLinkTooltip, the ScreenTip record that may follow an HLINK.
pub(crate) const HLINK_TOOLTIP: u16 = 0x0800;

/// StdLink `{79EAC9D0-BAF9-11CE-8C82-00AA004BA90B}`.
const STD_LINK: [u8; 16] = [
    0xD0, 0xC9, 0xEA, 0x79, 0xF9, 0xBA, 0xCE, 0x11, 0x8C, 0x82, 0x00, 0xAA, 0x00, 0x4B, 0xA9, 0x0B,
];
/// URL Moniker `{79EAC9E0-BAF9-11CE-8C82-00AA004BA90B}`.
const URL_MONIKER: [u8; 16] = [
    0xE0, 0xC9, 0xEA, 0x79, 0xF9, 0xBA, 0xCE, 0x11, 0x8C, 0x82, 0x00, 0xAA, 0x00, 0x4B, 0xA9, 0x0B,
];
/// File Moniker `{00000303-0000-0000-C000-000000000046}`.
const FILE_MONIKER: [u8; 16] = [
    0x03, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
];

/// The most `..\\` steps a File moniker's `cAnti` may name.
const MAX_ANTI: u16 = 255;

const HAS_MONIKER: u32 = 0x01;
const IS_ABSOLUTE: u32 = 0x02;
const HAS_LOCATION: u32 = 0x08;
const HAS_DISPLAY_NAME: u32 = 0x10;
const HAS_FRAME_NAME: u32 = 0x80;
const MONIKER_SAVED_AS_STR: u32 = 0x100;

/// A HyperlinkString: the character count with the null, the UTF-16
/// characters, the null.
fn put_string(out: &mut Vec<u8>, s: &str) {
    let units: Vec<u16> = s.encode_utf16().collect();
    out.extend_from_slice(&(units.len() as u32 + 1).to_le_bytes());
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
}

/// The HLINK body for `target` over `rect` (on the grid, so each bound fits
/// a `u16`).
pub(crate) fn body(rect: Rect, target: &str) -> Vec<u8> {
    let (r1, c1, r2, c2) = rect;
    let mut b = Vec::new();
    for v in [r1, r2, c1, c2] {
        b.extend_from_slice(&(v as u16).to_le_bytes());
    }
    b.extend_from_slice(&STD_LINK);
    b.extend_from_slice(&2u32.to_le_bytes());
    let mut rest = Vec::new();
    let (path, location) = if has_scheme(target) || target.starts_with('#') {
        (target, None)
    } else {
        split_location(target)
    };
    let flags = if let Some(location) = target.strip_prefix('#') {
        put_string(&mut rest, location);
        HAS_LOCATION
    } else if path.starts_with("\\\\") {
        put_string(&mut rest, path);
        HAS_MONIKER | IS_ABSOLUTE | MONIKER_SAVED_AS_STR
    } else if has_scheme(target) {
        rest.extend_from_slice(&URL_MONIKER);
        let units: Vec<u16> = target.encode_utf16().collect();
        rest.extend_from_slice(&((units.len() as u32 + 1) * 2).to_le_bytes());
        for u in units {
            rest.extend_from_slice(&u.to_le_bytes());
        }
        rest.extend_from_slice(&[0, 0]);
        HAS_MONIKER | IS_ABSOLUTE
    } else {
        file_moniker(&mut rest, path)
    };
    let flags = match location {
        Some(l) => {
            put_string(&mut rest, l);
            flags | HAS_LOCATION
        }
        None => flags,
    };
    b.extend_from_slice(&flags.to_le_bytes());
    b.extend_from_slice(&rest);
    b
}

/// A path's location in the file it names (a link to a cell of another
/// workbook), written after the moniker as Excel does: the tail after the
/// first `#` that reads as one, a reference (`Sheet1!A1`) or a defined name
/// (`MyName`), with no `\` or `/`. The model holds `path#loc` as one string,
/// so a file whose own name has such a `#` (`a#b.xlsx`) is still split;
/// `C:\Dev\C#\a.xlsx` and `a#1.xlsx` are not.
fn split_location(target: &str) -> (&str, Option<&str>) {
    for (at, _) in target.match_indices('#') {
        let (path, tail) = (&target[..at], &target[at + 1..]);
        if path.is_empty() || tail.is_empty() || tail.contains(['\\', '/']) {
            continue;
        }
        let name = tail.starts_with(|c: char| c.is_alphabetic() || c == '_')
            && tail
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
        if tail.contains('!') || name {
            return (path, Some(tail));
        }
    }
    (target, None)
}

/// A File moniker for a path with no scheme; the flags it takes.
fn file_moniker(out: &mut Vec<u8>, path: &str) -> u32 {
    let bytes = path.as_bytes();
    let absolute = path.starts_with('\\')
        || path.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':');
    let mut rest = path;
    let mut anti: u16 = 0;
    if !absolute {
        while let Some(r) = rest
            .strip_prefix("..\\")
            .or_else(|| rest.strip_prefix("../"))
        {
            // The reader takes no more; the rest stay in the path.
            if anti == MAX_ANTI {
                break;
            }
            anti += 1;
            rest = r;
        }
    }
    out.extend_from_slice(&FILE_MONIKER);
    out.extend_from_slice(&anti.to_le_bytes());
    let ansi: Vec<u8> = rest
        .chars()
        .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
        .collect();
    out.extend_from_slice(&(ansi.len() as u32 + 1).to_le_bytes());
    out.extend_from_slice(&ansi);
    out.push(0);
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&0xDEADu16.to_le_bytes());
    out.extend_from_slice(&[0; 20]);
    if rest.is_ascii() {
        out.extend_from_slice(&0u32.to_le_bytes());
    } else {
        let units: Vec<u16> = rest.encode_utf16().collect();
        let n = units.len() as u32 * 2;
        out.extend_from_slice(&(n + 6).to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&3u16.to_le_bytes());
        for u in units {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }
    if absolute {
        HAS_MONIKER | IS_ABSOLUTE
    } else {
        HAS_MONIKER
    }
}

/// The Hyperlink Object's own fields.
impl Le<'_> {
    /// A HyperlinkString, up to its first null.
    fn hyperlink_string(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        let units = self.take(n.checked_mul(2)?)?;
        Some(utf16_to_null(units))
    }
}

/// UTF-16LE `bytes` up to the first null unit, lossy.
fn utf16_to_null(bytes: &[u8]) -> String {
    let end = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .position(|p| *p == [0, 0])
        .map_or(bytes.len() & !1, |i| i * 2);
    utf16(&bytes[..end])
}

/// An HLINK body's rectangle and model target, or `None` for one this
/// reader can't read: a bad range (reversed, past column IV), an unknown
/// link or moniker class, a short or overlong field. Both a moniker and a
/// location make the location the URL's fragment (`url#loc`); the `.xlsx`
/// loader, which has them as separate attributes, keeps only the URL.
pub(crate) fn parse(body: &[u8]) -> Option<(Rect, String)> {
    let mut c = Le::new(body);
    let (r1, r2, c1, c2) = (c.u16()?, c.u16()?, c.u16()?, c.u16()?);
    if r1 > r2 || c1 > c2 || c2 > 255 {
        return None;
    }
    if c.take(16)? != STD_LINK || c.u32()? != 2 {
        return None;
    }
    let flags = c.u32()?;
    if flags & HAS_DISPLAY_NAME != 0 {
        c.hyperlink_string()?;
    }
    if flags & HAS_FRAME_NAME != 0 {
        c.hyperlink_string()?;
    }
    // A URL or string moniker is URI text, mapped as an xlsx Target is
    // ([`model_target`]); a File moniker's path is native, as the model holds
    // paths.
    let moniker = if flags & HAS_MONIKER == 0 {
        None
    } else if flags & MONIKER_SAVED_AS_STR != 0 {
        Some(model_target(&c.hyperlink_string()?).into_owned())
    } else {
        let class = c.take(16)?;
        if class == URL_MONIKER {
            let n = c.u32()? as usize;
            Some(model_target(&utf16_to_null(c.take(n)?)).into_owned())
        } else if class == FILE_MONIKER {
            Some(read_file_moniker(&mut c)?)
        } else {
            return None;
        }
    };
    let location = if flags & HAS_LOCATION != 0 {
        Some(c.hyperlink_string()?)
    } else {
        None
    };
    let target = match (moniker, location) {
        (Some(m), None) => m,
        (Some(m), Some(l)) => format!("{m}#{l}"),
        (None, Some(l)) => format!("#{l}"),
        (None, None) => return None,
    };
    if target.is_empty() {
        return None;
    }
    Some(((r1 as u32, c1 as u32, r2 as u32, c2 as u32), target))
}

/// A File moniker's path: `cAnti` `..\` segments, then the Unicode path
/// when there is one, else the ANSI path (its bytes read as Latin-1).
fn read_file_moniker(c: &mut Le) -> Option<String> {
    let anti = c.u16()?;
    // Excel climbs a few folders; more is a hostile record, not a path.
    if anti > MAX_ANTI {
        return None;
    }
    let n = c.u32()? as usize;
    let ansi = c.take(n)?;
    let ansi: String = ansi
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .collect();
    // endServer, versionNumber and the 20 reserved bytes.
    c.take(24)?;
    let size = c.u32()?;
    let path = if size == 0 {
        ansi
    } else {
        let bytes = c.u32()? as usize;
        if bytes.checked_add(6)? != size as usize || c.u16()? != 3 || !bytes.is_multiple_of(2) {
            return None;
        }
        utf16(c.take(bytes)?)
    };
    Some(format!("{}{path}", "..\\".repeat(anti as usize)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16z(s: &str) -> Vec<u8> {
        let mut v: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
        v.extend_from_slice(&[0, 0]);
        v
    }

    /// The fixed head: Ref8U (rows 1..2, columns 3..4), StdLink, version 2.
    fn head() -> Vec<u8> {
        let mut b = vec![1, 0, 2, 0, 3, 0, 4, 0];
        b.extend_from_slice(&STD_LINK);
        b.extend_from_slice(&[2, 0, 0, 0]);
        b
    }

    #[test]
    fn hlink_body_bytes() {
        let rect = (1, 3, 2, 4);
        // A URL: flags 3, the URL moniker, its byte length with the null.
        let mut want = head();
        want.extend_from_slice(&[3, 0, 0, 0]);
        want.extend_from_slice(&[
            0xE0, 0xC9, 0xEA, 0x79, 0xF9, 0xBA, 0xCE, 0x11, 0x8C, 0x82, 0x00, 0xAA, 0x00, 0x4B,
            0xA9, 0x0B,
        ]);
        want.extend_from_slice(&[10, 0, 0, 0]);
        want.extend_from_slice(&utf16z("ab:c"));
        assert_eq!(body(rect, "ab:c"), want);
        // A location: flags 8, its character count with the null.
        let mut want = head();
        want.extend_from_slice(&[8, 0, 0, 0, 5, 0, 0, 0]);
        want.extend_from_slice(&utf16z("S!B3"));
        assert_eq!(body(rect, "#S!B3"), want);
        // A UNC path: flags 0x103, the path as a string.
        let mut want = head();
        want.extend_from_slice(&[3, 1, 0, 0, 6, 0, 0, 0]);
        want.extend_from_slice(&utf16z("\\\\s\\t"));
        assert_eq!(body(rect, "\\\\s\\t"), want);
        // A relative path: flags 1, File moniker, cAnti 1, the ANSI path,
        // 0xFFFF, 0xDEAD, 20 zero bytes, no Unicode extension.
        let mut want = head();
        want.extend_from_slice(&[1, 0, 0, 0]);
        want.extend_from_slice(&[0x03, 0x03, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
        want.extend_from_slice(&[1, 0, 4, 0, 0, 0, b'a', b'\\', b'b', 0]);
        want.extend_from_slice(&[0xFF, 0xFF, 0xAD, 0xDE]);
        want.extend_from_slice(&[0; 20]);
        want.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(body(rect, "..\\a\\b"), want);
        // A non-ASCII absolute path: flags 3, `?` in the ANSI path, the
        // Unicode extension (size = bytes + 6, the bytes, key 3, no null).
        let mut want = head();
        want.extend_from_slice(&[3, 0, 0, 0]);
        want.extend_from_slice(&[0x03, 0x03, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
        want.extend_from_slice(&[0, 0, 4, 0, 0, 0, b'C', b':', b'?', 0]);
        want.extend_from_slice(&[0xFF, 0xFF, 0xAD, 0xDE]);
        want.extend_from_slice(&[0; 20]);
        want.extend_from_slice(&[12, 0, 0, 0, 6, 0, 0, 0, 3, 0]);
        want.extend_from_slice(&[b'C', 0, b':', 0, 0xE9, 0]);
        assert_eq!(body(rect, "C:é"), want);
    }

    #[test]
    fn written_bodies_parse_back() {
        for t in [
            "https://example.com/a?b=1",
            "mailto:a@b.example",
            "#Sheet2!B3",
            "#'My Sheet'!A1:B2",
            "\\\\srv\\share\\a.xlsx",
            "C:\\x\\y.xlsx",
            "\\dir\\a.xlsx",
            "..\\up\\a.xlsx",
            "..\\..\\a.xlsx",
            "a.xlsx",
            "C:\\é\\ü.xlsx",
            "..\\é.xlsx",
            "file:///C:/x",
            "..\\a.xls#Sheet1!A1",
            "C:\\b.xlsx#S!A1",
            "\\\\s\\sh\\a.xls#S!A1",
            "https://x/#frag",
            "C:\\b.xlsx#MyName",
            "#'Q#1'!A1",
            "#a#b",
            "C:\\Dev\\C#\\a.xlsx",
            "\\\\s\\sh\\a#1.xlsx",
        ] {
            assert_eq!(
                parse(&body((5, 1, 9, 2), t)),
                Some(((5, 1, 9, 2), t.into())),
                "{t}"
            );
        }
        // Spellings the model holds otherwise read as the model's.
        assert_eq!(
            parse(&body((0, 0, 0, 0), "file://srv/share/x")).map(|l| l.1),
            Some("\\\\srv\\share\\x".into())
        );
        assert_eq!(
            parse(&body((0, 0, 0, 0), "file:///\\\\srv\\share\\x")).map(|l| l.1),
            Some("\\\\srv\\share\\x".into())
        );
    }

    #[test]
    fn an_internal_location_with_a_hash_is_one_string() {
        for loc in ["'Q#1'!A1", "a#b"] {
            let mut want = head();
            want.extend_from_slice(&HAS_LOCATION.to_le_bytes());
            want.extend_from_slice(&(loc.encode_utf16().count() as u32 + 1).to_le_bytes());
            want.extend_from_slice(&utf16z(loc));
            assert_eq!(body((1, 3, 2, 4), &format!("#{loc}")), want, "{loc}");
        }
    }

    #[test]
    fn an_escaped_hash_path_is_one_file_moniker() {
        let b = body((0, 0, 0, 0), "Plan%23Final.xlsx");
        let flags = u32::from_le_bytes(b[28..32].try_into().unwrap());
        assert_eq!(flags, HAS_MONIKER);
        assert_eq!(&b[32..48], &FILE_MONIKER);
        assert_eq!(parse(&b).unwrap().1, "Plan%23Final.xlsx");
    }

    #[test]
    fn a_file_path_is_native_text() {
        // A File moniker's path is native, as the model holds paths: a `%` is
        // a `%` (an xlsx Target's escapes are decoded on load, #1726 r5).
        for t in ["C:\\R\\Sales%2024.xlsx", "..\\a%41.xlsx", "My%20Book.xlsx"] {
            assert_eq!(parse(&body((0, 0, 0, 0), t)).unwrap().1, t);
        }
    }

    #[test]
    fn c_anti_stops_where_the_reader_does() {
        let t = format!("{}a.xlsx", "..\\".repeat(256));
        let b = body((0, 0, 0, 0), &t);
        // cAnti 255, the 256th `..\` left in the path.
        assert_eq!(&b[48..50], &255u16.to_le_bytes());
        assert_eq!(parse(&b).unwrap().1, t);
    }

    #[test]
    fn only_a_location_shaped_tail_is_split() {
        let flags = |t: &str| u32::from_le_bytes(body((0, 0, 0, 0), t)[28..32].try_into().unwrap());
        for t in [
            "C:\\Dev\\C#\\a.xlsx",
            "\\\\s\\sh\\a#1.xlsx",
            "a#",
            "C:\\x#y\\z!A1",
        ] {
            assert_eq!(flags(t) & HAS_LOCATION, 0, "{t}");
        }
        for t in [
            "..\\a.xls#Sheet1!A1",
            "C:\\b.xlsx#MyName",
            "\\\\s\\sh\\a.xls#'S 1'!A1",
        ] {
            assert_ne!(flags(t) & HAS_LOCATION, 0, "{t}");
        }
        assert_eq!(
            split_location("C:\\b.xlsx#S!A1"),
            ("C:\\b.xlsx", Some("S!A1"))
        );
    }

    #[test]
    fn hlink_url_file_and_location_monikers_read() {
        // A display name and a frame (skipped), a URL moniker with a
        // serialGUID trailer, and a location: the URL with a fragment.
        let mut b = head();
        b.extend_from_slice(
            &(HAS_MONIKER | IS_ABSOLUTE | 0x14 | HAS_FRAME_NAME | HAS_LOCATION).to_le_bytes(),
        );
        b.extend_from_slice(&[3, 0, 0, 0]);
        b.extend_from_slice(&utf16z("ab"));
        b.extend_from_slice(&[2, 0, 0, 0]);
        b.extend_from_slice(&utf16z("f"));
        b.extend_from_slice(&URL_MONIKER);
        let url = utf16z("http://x/");
        b.extend_from_slice(&(url.len() as u32 + 24).to_le_bytes());
        b.extend_from_slice(&url);
        b.extend_from_slice(&[0x77; 24]);
        b.extend_from_slice(&[4, 0, 0, 0]);
        b.extend_from_slice(&utf16z("top"));
        assert_eq!(parse(&b), Some(((1, 3, 2, 4), "http://x/#top".into())));

        // A File moniker with cAnti 2 and a Unicode extension that wins over
        // the ANSI path.
        let mut b = head();
        b.extend_from_slice(&HAS_MONIKER.to_le_bytes());
        b.extend_from_slice(&FILE_MONIKER);
        b.extend_from_slice(&[2, 0, 3, 0, 0, 0, b'?', b'x', 0]);
        b.extend_from_slice(&[0xFF, 0xFF, 0xAD, 0xDE]);
        b.extend_from_slice(&[0; 20]);
        b.extend_from_slice(&[10, 0, 0, 0, 4, 0, 0, 0, 3, 0]);
        b.extend_from_slice(&[0xE9, 0, b'x', 0]);
        assert_eq!(parse(&b), Some(((1, 3, 2, 4), "..\\..\\éx".into())));
        // A File moniker and a location: a cell in another workbook.
        let mut b = head();
        b.extend_from_slice(&(HAS_MONIKER | HAS_LOCATION).to_le_bytes());
        b.extend_from_slice(&FILE_MONIKER);
        b.extend_from_slice(&[0, 0, 10, 0, 0, 0]);
        b.extend_from_slice(b"Book2.xls\0");
        b.extend_from_slice(&[0xFF, 0xFF, 0xAD, 0xDE]);
        b.extend_from_slice(&[0; 20]);
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(&[10, 0, 0, 0]);
        b.extend_from_slice(&utf16z("Sheet1!A1"));
        assert_eq!(
            parse(&b),
            Some(((1, 3, 2, 4), "Book2.xls#Sheet1!A1".into()))
        );
        // And it is what the writer writes for that target.
        assert_eq!(body((1, 3, 2, 4), "Book2.xls#Sheet1!A1"), b);
        // A cAnti past 255 is no path.
        let mut b = body((1, 3, 2, 4), "..\\a");
        assert_eq!(parse(&b).map(|l| l.1), Some("..\\a".into()));
        b[48..50].copy_from_slice(&256u16.to_le_bytes());
        assert_eq!(parse(&b), None);
        b[48..50].copy_from_slice(&255u16.to_le_bytes());
        assert!(parse(&b).is_some());
        // Without the extension the ANSI path is read as Latin-1.
        let mut b = head();
        b.extend_from_slice(&HAS_MONIKER.to_le_bytes());
        b.extend_from_slice(&FILE_MONIKER);
        b.extend_from_slice(&[0, 0, 3, 0, 0, 0, 0xE9, b'x', 0]);
        b.extend_from_slice(&[0xFF, 0xFF, 0xAD, 0xDE]);
        b.extend_from_slice(&[0; 20]);
        b.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(parse(&b), Some(((1, 3, 2, 4), "éx".into())));
        // A string moniker spelled as a file URI is the model's UNC path.
        let mut b = head();
        b.extend_from_slice(&(HAS_MONIKER | MONIKER_SAVED_AS_STR).to_le_bytes());
        let s = "file://srv/share/a";
        b.extend_from_slice(&(s.len() as u32 + 1).to_le_bytes());
        b.extend_from_slice(&utf16z(s));
        assert_eq!(parse(&b), Some(((1, 3, 2, 4), "\\\\srv\\share\\a".into())));
    }

    #[test]
    fn unreadable_hlinks_are_none() {
        let good = body((1, 3, 2, 4), "https://x/");
        assert!(parse(&good).is_some());
        // Truncated anywhere.
        for n in 0..good.len() {
            assert_eq!(parse(&good[..n]), None, "{n}");
        }
        // Reversed rows, reversed columns, a column past IV.
        for (i, v) in [(0, 3u16), (4, 5), (6, 256)] {
            let mut b = good.clone();
            b[i..i + 2].copy_from_slice(&v.to_le_bytes());
            assert_eq!(parse(&b), None, "{i}");
        }
        // Another link class, another stream version, another moniker.
        for i in [8, 24, 32] {
            let mut b = good.clone();
            b[i] ^= 1;
            assert_eq!(parse(&b), None, "{i}");
        }
        // A string length past the body.
        let mut b = body((0, 0, 0, 0), "#A1");
        b[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse(&b), None);
        // Neither a moniker nor a location.
        let mut b = head();
        b.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(parse(&b), None);
    }
}
