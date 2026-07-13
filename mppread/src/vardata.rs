//! The MPP **Var2Data** block stream — a documented container inside a `.mpp`.
//!
//! Microsoft Project stores variable-length values (task/resource names, notes,
//! text fields, …) in a `Var2Data` stream as a flat sequence of length-prefixed
//! blocks: a little-endian `u32` byte-count, then that many bytes. A companion
//! `VarMeta` stream indexes which block belongs to which item and field; this
//! module handles the block *container* (enumerate blocks, pull the UTF-16
//! strings) — the semantic mapping to task fields is a later layer that needs
//! the version-specific field ids.
//!
//! Verified against real `.mpp` files: the task names come straight out as
//! readable UTF-16LE strings.

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Split a `Var2Data` stream into its length-prefixed blocks (slices into the
/// input). Zero-length blocks are skipped; a truncated final block ends parsing.
pub fn blocks(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= data.len() {
        let len = u32le(data, i) as usize;
        i += 4;
        if len == 0 {
            continue;
        }
        if i + len > data.len() {
            break;
        }
        out.push(&data[i..i + len]);
        i += len;
    }
    out
}

/// Like [`blocks`] but also returns each block's byte offset within the stream
/// (the value a `VarMeta` entry points at). Useful for correlating the two.
pub fn block_offsets(data: &[u8]) -> Vec<(usize, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= data.len() {
        let len = u32le(data, i) as usize;
        let start = i;
        i += 4;
        if len == 0 {
            continue;
        }
        if i + len > data.len() {
            break;
        }
        out.push((start, &data[i..i + len]));
        i += len;
    }
    out
}

/// Read the length-prefixed block at an explicit byte `offset` (the value a
/// `VarMeta` entry points at) and decode it as a UTF-16 string if it is one.
///
/// Unlike [`block_offsets`], this doesn't assume the stream is one contiguous
/// run of blocks: newer `.mpp` `Var2Data` streams have gaps and reordering, so a
/// sequential walk stalls and misses most blocks. The `VarMeta` offsets are the
/// authoritative index, so reading each block *at* its offset finds them all.
pub fn string_at(data: &[u8], offset: usize) -> Option<String> {
    if offset + 4 > data.len() {
        return None;
    }
    let len = u32le(data, offset) as usize;
    let start = offset + 4;
    if len < 2 || start + len > data.len() {
        return None;
    }
    utf16le_string(&data[start..start + len])
}

/// Decode a block as a UTF-16LE string if it plausibly is one (even length,
/// ≥80% printable, non-empty), stripping a trailing NUL terminator.
pub fn utf16le_string(b: &[u8]) -> Option<String> {
    if b.len() < 2 || !b.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    let s = String::from_utf16(&units[..end]).ok()?;
    let total = s.chars().count();
    if total == 0 || s.chars().any(|c| c.is_control()) {
        return None;
    }
    // Keep ASCII-heavy text (real Latin names) or reasonably long runs (so
    // non-Latin names survive) — but reject the 1–2 char blobs that binary
    // metadata blocks decode into.
    let ascii = s.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').count();
    (ascii * 10 >= total * 6 || total >= 4).then_some(s)
}

/// Every block in a `Var2Data` stream that decodes as a readable UTF-16 string,
/// in stream order. For a task `Var2Data` this surfaces the task names (and
/// other text fields) — the first useful decode of real `.mpp` content.
pub fn strings(data: &[u8]) -> Vec<String> {
    blocks(data).into_iter().filter_map(utf16le_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `[u32 len][bytes]` block.
    fn block(bytes: &[u8]) -> Vec<u8> {
        let mut v = (bytes.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(bytes);
        v
    }
    fn utf16(s: &str) -> Vec<u8> {
        let mut v: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        v.extend_from_slice(&[0, 0]); // NUL terminator, as MPP writes
        v
    }

    #[test]
    fn splits_length_prefixed_blocks() {
        let mut data = Vec::new();
        data.extend_from_slice(&block(&utf16("General Conditions")));
        data.extend_from_slice(&block(&[0x0c, 0x00, 0xc0, 0x12])); // a binary block
        data.extend_from_slice(&block(&utf16("Submit bond")));
        let bl = blocks(&data);
        assert_eq!(bl.len(), 3);
    }

    #[test]
    fn extracts_utf16_task_names() {
        // mirrors the real Commercial-Construction layout: names interleaved
        // with small binary metadata blocks
        let mut data = Vec::new();
        data.extend_from_slice(&block(&utf16("Commercial Construction")));
        data.extend_from_slice(&block(&utf16("Three-story Office Building")));
        data.extend_from_slice(&block(&[0x0c, 0x00, 0xc0, 0x12, 0x00, 0x00, 0x27, 0x00]));
        data.extend_from_slice(&block(&utf16("General Conditions")));
        let names = strings(&data);
        assert_eq!(
            names,
            vec![
                "Commercial Construction".to_string(),
                "Three-story Office Building".to_string(),
                "General Conditions".to_string(),
            ]
        );
    }

    #[test]
    fn reads_block_at_explicit_offset() {
        // A non-contiguous stream: a gap, then a block at a known offset that a
        // sequential walk would never reach cleanly.
        let mut data = vec![0xAAu8; 10]; // junk gap
        let off = data.len();
        data.extend_from_slice(&block(&utf16("Modeling")));
        assert_eq!(string_at(&data, off).as_deref(), Some("Modeling"));
        assert_eq!(string_at(&data, 0), None); // gap isn't a valid string block
        assert_eq!(string_at(&data, data.len()), None); // past the end
    }

    #[test]
    fn rejects_binary_and_truncated() {
        assert!(utf16le_string(&[0x01, 0x00, 0x02, 0x00]).is_none()); // control chars
        assert!(utf16le_string(&[0x41]).is_none()); // odd length
        assert!(utf16le_string(&[0xc0, 0x12]).is_none()); // 1-char non-ASCII blob (real MPP noise)
        // a truncated trailing block is dropped
        let mut data = block(&utf16("ok"));
        data.extend_from_slice(&[0x40, 0x00, 0x00, 0x00, 0x41]); // claims 64 bytes, has 1
        assert_eq!(strings(&data), vec!["ok".to_string()]);
    }
}
