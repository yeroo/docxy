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
use crate::xlsx::model_target;

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

const HAS_MONIKER: u32 = 0x01;
const IS_ABSOLUTE: u32 = 0x02;
const HAS_LOCATION: u32 = 0x08;
const HAS_DISPLAY_NAME: u32 = 0x10;
const HAS_FRAME_NAME: u32 = 0x80;
const MONIKER_SAVED_AS_STR: u32 = 0x100;

/// Whether `target` starts with a URI scheme of two or more characters
/// (`C:` is a drive, not a scheme).
fn has_scheme(target: &str) -> bool {
    let Some(colon) = target.find(':') else {
        return false;
    };
    let scheme = &target[..colon];
    scheme.len() >= 2
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

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
    // A path's `#` starts a location in the file it names (a link to a cell
    // of another workbook), written after the moniker as Excel does.
    let (path, location) = match target.split_once('#') {
        Some((p, l)) if !p.is_empty() && !has_scheme(target) => (p, Some(l)),
        _ => (target, None),
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
            if anti == u16::MAX {
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

/// UTF-16LE `bytes` up to the first null, lossy.
fn utf16_to_null(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
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
    let moniker = if flags & HAS_MONIKER == 0 {
        None
    } else if flags & MONIKER_SAVED_AS_STR != 0 {
        Some(c.hyperlink_string()?)
    } else {
        let class = c.take(16)?;
        if class == URL_MONIKER {
            let n = c.u32()? as usize;
            Some(utf16_to_null(c.take(n)?))
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
        (Some(m), None) => model_target(&m).into_owned(),
        (Some(m), Some(l)) => format!("{}#{l}", model_target(&m)),
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
        let units: Vec<u16> = c
            .take(bytes)?
            .chunks_exact(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        String::from_utf16_lossy(&units)
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
