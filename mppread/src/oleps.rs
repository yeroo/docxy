//! Reader for OLE **Property Sets** (MS-OLEPS), the documented format behind the
//! `\x05SummaryInformation` and `\x05DocumentSummaryInformation` streams found in
//! every compound file — `.doc`, `.xls`, and `.mpp` alike.
//!
//! A property set is a tiny typed key/value store: a header naming one or two
//! *sections*, each an index of `(property-id, offset)` pairs pointing at typed
//! values (strings, integers, timestamps). Property ids are interpreted by the
//! stream they live in (title, author, company, …). This is the one part of a
//! `.mpp`'s contents that is fully documented, so it decodes exactly.

use std::collections::HashMap;

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(v)
}

/// A decoded property value (only the variant types we need are modeled).
#[derive(Clone, Debug, PartialEq)]
pub enum Prop {
    Str(String),
    Int(i32),
    /// FILETIME: 100-nanosecond intervals since 1601-01-01 UTC.
    Time(u64),
}

impl Prop {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Prop::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_time(&self) -> Option<u64> {
        match self {
            Prop::Time(t) => Some(*t),
            _ => None,
        }
    }
}

// VARIANT type tags (low bytes of the 4-byte type field).
const VT_I2: u32 = 2;
const VT_I4: u32 = 3;
const VT_BOOL: u32 = 11;
const VT_LPSTR: u32 = 30; // length-prefixed code-page string
const VT_LPWSTR: u32 = 31; // length-prefixed UTF-16 string
const VT_FILETIME: u32 = 64;

/// Parse the **first section** of a property-set stream into an id → value map.
/// Unknown value types are skipped rather than failing the whole parse.
pub fn parse(stream: &[u8]) -> Option<HashMap<u32, Prop>> {
    if stream.len() < 48 || u16le(stream, 0) != 0xFFFE {
        return None; // not a little-endian property set
    }
    let num_sections = u32le(stream, 24);
    if num_sections == 0 {
        return None;
    }
    // First section: FMTID (16) at 28, its offset (4) at 44.
    let sec = u32le(stream, 44) as usize;
    if sec + 8 > stream.len() {
        return None;
    }
    let count = u32le(stream, sec + 4) as usize;
    let entries: Vec<(u32, usize)> = (0..count)
        .map(|i| sec + 8 + i * 8)
        .take_while(|idx| idx + 8 <= stream.len())
        .map(|idx| (u32le(stream, idx), sec + u32le(stream, idx + 4) as usize))
        .collect();
    // The code page of the section's VT_LPSTR values (PID_CODEPAGE, property
    // 1, a VT_I2 Project writes before the strings), 1252 when absent.
    let page = entries
        .iter()
        .find(|(pid, _)| *pid == PID_CODEPAGE)
        .and_then(|(_, off)| read_value(stream, *off, DEFAULT_CODEPAGE))
        .and_then(|p| match p {
            Prop::Int(n) => Some(n),
            _ => None,
        })
        .unwrap_or(DEFAULT_CODEPAGE);
    let mut out = HashMap::new();
    for (pid, off) in entries {
        if let Some(p) = read_value(stream, off, page) {
            out.insert(pid, p);
        }
    }
    Some(out)
}

/// `PID_CODEPAGE`: the code page of the section's `VT_LPSTR` strings.
const PID_CODEPAGE: u32 = 1;
/// The code page assumed when a section names none (Windows-1252, what
/// Project writes on a Western-locale machine).
const DEFAULT_CODEPAGE: i32 = 1252;

/// A `VT_LPSTR` string's bytes in code page `page`: through
/// `opccore::codepage` for the pages it knows (a byte the page leaves
/// undefined, or a page it does not know, reads as Latin-1, as before).
fn decode_lpstr(bytes: &[u8], page: i32) -> String {
    bytes
        .iter()
        .map(|&c| opccore::codepage::decode(page, c).unwrap_or(char::from(c)))
        .collect()
}

/// Read one typed value at `off` (the 4-byte type tag followed by its
/// payload); `page` decodes a `VT_LPSTR`.
fn read_value(b: &[u8], off: usize, page: i32) -> Option<Prop> {
    if off + 4 > b.len() {
        return None;
    }
    let ty = u32le(b, off) & 0xFFFF; // strip VT_VECTOR/VT_ARRAY flags
    let v = off + 4;
    match ty {
        VT_I2 => (v + 2 <= b.len()).then(|| Prop::Int(u16le(b, v) as i16 as i32)),
        VT_I4 => (v + 4 <= b.len()).then(|| Prop::Int(u32le(b, v) as i32)),
        VT_BOOL => (v + 2 <= b.len()).then(|| Prop::Int((u16le(b, v) != 0) as i32)),
        VT_FILETIME => (v + 8 <= b.len()).then(|| Prop::Time(u64le(b, v))),
        VT_LPSTR => {
            let n = u32le(b, v) as usize;
            let s = v + 4;
            (s + n <= b.len()).then(|| {
                // A code-page string (the section's PID_CODEPAGE: an em dash
                // is 0x97 in 1252, not U+0097), up to its NUL.
                let bytes = &b[s..s + n];
                let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
                Prop::Str(decode_lpstr(&bytes[..end], page))
            })
        }
        VT_LPWSTR => {
            let n = u32le(b, v) as usize; // count of UTF-16 code units incl NUL
            let s = v + 4;
            (s + n * 2 <= b.len()).then(|| {
                let units: Vec<u16> = (0..n).map(|i| u16le(b, s + i * 2)).collect();
                let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
                Prop::Str(String::from_utf16_lossy(&units[..end]))
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value as a test writes it: a `Prop`, or a `VT_LPSTR` of the given
    /// bytes (a code-page string a `Prop::Str` cannot spell), or a `VT_I2`.
    enum Raw {
        Prop(Prop),
        Lpstr(Vec<u8>),
        I2(i16),
    }

    /// Build a one-section property-set stream from `(id, Prop)` pairs.
    fn build(props: &[(u32, Prop)]) -> Vec<u8> {
        let raw: Vec<(u32, Raw)> = props
            .iter()
            .map(|(id, p)| (*id, Raw::Prop(p.clone())))
            .collect();
        build_raw(&raw)
    }

    fn build_raw(props: &[(u32, Raw)]) -> Vec<u8> {
        // Section body: [cb u32][count u32][index: (id,off) …][values …]
        let count = props.len();
        let index_len = 8 + count * 8;
        let mut body_index = Vec::new();
        let mut values = Vec::new();
        let lpstr = |values: &mut Vec<u8>, text: &[u8]| {
            let mut bytes: Vec<u8> = text.to_vec();
            bytes.push(0);
            while !bytes.len().is_multiple_of(4) {
                bytes.push(0);
            }
            values.extend_from_slice(&VT_LPSTR.to_le_bytes());
            values.extend_from_slice(&((text.len() + 1) as u32).to_le_bytes());
            values.extend_from_slice(&bytes);
        };
        for (id, p) in props {
            let off = index_len + values.len();
            body_index.extend_from_slice(&id.to_le_bytes());
            body_index.extend_from_slice(&(off as u32).to_le_bytes());
            match p {
                Raw::Prop(Prop::Int(i)) => {
                    values.extend_from_slice(&VT_I4.to_le_bytes());
                    values.extend_from_slice(&(*i as u32).to_le_bytes());
                }
                Raw::Prop(Prop::Time(t)) => {
                    values.extend_from_slice(&VT_FILETIME.to_le_bytes());
                    values.extend_from_slice(&t.to_le_bytes());
                }
                Raw::Prop(Prop::Str(s)) => lpstr(&mut values, s.as_bytes()),
                Raw::Lpstr(bytes) => lpstr(&mut values, bytes),
                Raw::I2(n) => {
                    values.extend_from_slice(&VT_I2.to_le_bytes());
                    values.extend_from_slice(&(*n as u16).to_le_bytes());
                    values.extend_from_slice(&[0, 0]);
                }
            }
        }
        let cb = 8 + body_index.len() + values.len();
        let mut section = Vec::new();
        section.extend_from_slice(&(cb as u32).to_le_bytes());
        section.extend_from_slice(&(count as u32).to_le_bytes());
        section.extend_from_slice(&body_index);
        section.extend_from_slice(&values);

        let mut out = Vec::new();
        out.extend_from_slice(&0xFFFEu16.to_le_bytes()); // byte order
        out.extend_from_slice(&0u16.to_le_bytes()); // version
        out.extend_from_slice(&0u32.to_le_bytes()); // system id
        out.extend_from_slice(&[0u8; 16]); // CLSID
        out.extend_from_slice(&1u32.to_le_bytes()); // one section
        out.extend_from_slice(&[0u8; 16]); // FMTID
        out.extend_from_slice(&48u32.to_le_bytes()); // section offset
        assert_eq!(out.len(), 48);
        out.extend_from_slice(&section);
        out
    }

    #[test]
    fn parses_strings_ints_and_time() {
        let stream = build(&[
            (2, Prop::Str("Demo Plan".into())),
            (4, Prop::Str("Alice".into())),
            (14, Prop::Int(42)),
            (12, Prop::Time(132_000_000_000_000_000)),
        ]);
        let m = parse(&stream).unwrap();
        assert_eq!(m[&2].as_str(), Some("Demo Plan"));
        assert_eq!(m[&4].as_str(), Some("Alice"));
        assert_eq!(m[&14], Prop::Int(42));
        assert_eq!(m[&12].as_time(), Some(132_000_000_000_000_000));
    }

    /// Project writes the strings in the section's code page (`PID_CODEPAGE`,
    /// a `VT_I2`): an em dash is 0x97 and curly quotes 0x93 0x94 in 1252,
    /// which Latin-1 read as C1 controls (issue 1514, a real Project file).
    #[test]
    fn lpstr_reads_through_the_sections_code_page() {
        let title = b"Plan \x97 \x93Phase 2\x94 caf\xe9".to_vec();
        let stream = build_raw(&[(1, Raw::I2(1252)), (2, Raw::Lpstr(title.clone()))]);
        let m = parse(&stream).unwrap();
        assert_eq!(
            m[&2].as_str(),
            Some("Plan \u{2014} \u{201c}Phase 2\u{201d} caf\u{e9}")
        );
        assert_eq!(m[&1], Prop::Int(1252));
        // Another page reads its own characters: 0xe9 is Cyrillic in 1251.
        let stream = build_raw(&[
            (1, Raw::I2(1251)),
            (2, Raw::Lpstr(b"caf\xe9 \x97".to_vec())),
        ]);
        assert_eq!(
            parse(&stream).unwrap()[&2].as_str(),
            Some("caf\u{0439} \u{2014}")
        );
        // No PID_CODEPAGE: 1252 is assumed.
        let stream = build_raw(&[(2, Raw::Lpstr(title))]);
        assert_eq!(
            parse(&stream).unwrap()[&2].as_str(),
            Some("Plan \u{2014} \u{201c}Phase 2\u{201d} caf\u{e9}")
        );
    }

    #[test]
    fn rejects_non_property_set() {
        assert!(parse(b"nope").is_none());
    }
}
