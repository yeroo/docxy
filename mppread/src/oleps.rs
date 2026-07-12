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
    let mut out = HashMap::new();
    for i in 0..count {
        let idx = sec + 8 + i * 8;
        if idx + 8 > stream.len() {
            break;
        }
        let pid = u32le(stream, idx);
        let off = sec + u32le(stream, idx + 4) as usize;
        if let Some(p) = read_value(stream, off) {
            out.insert(pid, p);
        }
    }
    Some(out)
}

/// Read one typed value at `off` (the 4-byte type tag followed by its payload).
fn read_value(b: &[u8], off: usize) -> Option<Prop> {
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
                // code-page string; decode as Latin-1 (fine for ASCII), drop NUL
                let bytes = &b[s..s + n];
                let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
                Prop::Str(bytes[..end].iter().map(|&c| c as char).collect())
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

    /// Build a one-section property-set stream from `(id, Prop)` pairs.
    fn build(props: &[(u32, Prop)]) -> Vec<u8> {
        // Section body: [cb u32][count u32][index: (id,off) …][values …]
        let count = props.len();
        let index_len = 8 + count * 8;
        let mut body_index = Vec::new();
        let mut values = Vec::new();
        for (id, p) in props {
            let off = index_len + values.len();
            body_index.extend_from_slice(&id.to_le_bytes());
            body_index.extend_from_slice(&(off as u32).to_le_bytes());
            match p {
                Prop::Int(i) => {
                    values.extend_from_slice(&VT_I4.to_le_bytes());
                    values.extend_from_slice(&(*i as u32).to_le_bytes());
                }
                Prop::Time(t) => {
                    values.extend_from_slice(&VT_FILETIME.to_le_bytes());
                    values.extend_from_slice(&t.to_le_bytes());
                }
                Prop::Str(s) => {
                    let mut bytes: Vec<u8> = s.bytes().collect();
                    bytes.push(0);
                    while !bytes.len().is_multiple_of(4) {
                        bytes.push(0);
                    }
                    values.extend_from_slice(&VT_LPSTR.to_le_bytes());
                    values.extend_from_slice(&((s.len() + 1) as u32).to_le_bytes());
                    values.extend_from_slice(&bytes);
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

    #[test]
    fn rejects_non_property_set() {
        assert!(parse(b"nope").is_none());
    }
}
