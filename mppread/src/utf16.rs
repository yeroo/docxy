//! The one UTF-16LE decoder of the strict readers (#438): task, resource and
//! calendar names, keyed text and the Props strings. Each caller keeps its
//! own rules on top (empty, control characters, padding) and its own error
//! text; this module only turns bytes into code units and units into text.

/// Why bytes are not the UTF-16 a caller wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Utf16Error {
    /// An odd length, or fewer bytes than the caller's minimum.
    Length,
    /// No NUL code unit where one must end the text.
    Unterminated,
    /// An unpaired surrogate.
    Invalid,
}

/// The little-endian code units of `bytes`, which must have an even length
/// of at least `min` bytes.
pub(crate) fn units(bytes: &[u8], min: usize) -> Result<Vec<u16>, Utf16Error> {
    if bytes.len() < min || !bytes.len().is_multiple_of(2) {
        return Err(Utf16Error::Length);
    }
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect())
}

/// `units` as text.
pub(crate) fn text(units: &[u16]) -> Result<String, Utf16Error> {
    String::from_utf16(units).map_err(|_| Utf16Error::Invalid)
}

/// Text that ends in exactly one NUL code unit as its last unit (the NUL is
/// not part of the result); `bytes` hold at least `min` bytes.
pub(crate) fn terminated(bytes: &[u8], min: usize) -> Result<String, Utf16Error> {
    let units = units(bytes, min.max(2))?;
    match units.split_last() {
        Some((0, body)) => text(body),
        _ => Err(Utf16Error::Unterminated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn terminated_text_drops_its_nul() {
        assert_eq!(terminated(&bytes("ab\0"), 2), Ok("ab".into()));
        assert_eq!(terminated(&bytes("\0"), 2), Ok(String::new()));
    }

    #[test]
    fn refusals_name_the_reason() {
        assert_eq!(terminated(&[0x41], 2), Err(Utf16Error::Length));
        assert_eq!(terminated(&bytes("\0"), 4), Err(Utf16Error::Length));
        assert_eq!(terminated(&bytes("ab"), 2), Err(Utf16Error::Unterminated));
        assert_eq!(terminated(&[0x00, 0xd8, 0, 0], 2), Err(Utf16Error::Invalid));
        assert_eq!(units(&[], 2), Err(Utf16Error::Length));
    }
}
