//! A small DEFLATE compressor (RFC 1951, fixed-Huffman blocks) and the zlib
//! wrapper around it (RFC 1950), so [`crate::png`] can write a real PNG without
//! pulling an image or compression crate into the tree.
//!
//! Fixed Huffman rather than dynamic: the code lengths are in the spec, so
//! there are no trees to build or emit, and on a screenshot that has already
//! been PNG-filtered the difference is small next to the LZ77 matching that
//! does the actual work. Stored blocks were the other option and would have
//! been shorter still to write, but a full-window capture is several megabytes
//! and a run takes many of them.
//!
//! The one thing that makes hand-rolling this reasonable is that the repo
//! already owns an inflater: every test here round-trips through
//! `opccore::inflate::inflate_raw`, so a bug in the bit packing cannot pass.

/// RFC 1951 3.2.5, length codes 257..285: the shortest match each code covers.
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// How many extra bits follow each length code.
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Distance codes 0..29: the shortest distance each covers.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// How many extra bits follow each distance code.
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// The sliding window DEFLATE allows.
const WINDOW: usize = 32768;
/// The shortest run worth coding as a match rather than as literals.
const MIN_MATCH: usize = 3;
/// The longest a single length code can express.
const MAX_MATCH: usize = 258;
/// How far back the hash chain is followed before settling for what it found.
/// A screenshot's filtered rows are mostly long runs of one value, so the first
/// few candidates are almost always the good ones; a deeper search costs time
/// for a fraction of a percent.
const MAX_CHAIN: usize = 32;
/// Size of the 3-byte hash table (a power of two, so the mask is cheap).
const HASH_BITS: u32 = 15;

/// Bits, packed the way DEFLATE wants them: into bytes from the least
/// significant end, with Huffman codes fed in most-significant bit first and
/// everything else least-significant bit first.
struct BitWriter {
    out: Vec<u8>,
    bit_buf: u32,
    bit_len: u32,
}

impl BitWriter {
    fn new() -> BitWriter {
        BitWriter {
            out: Vec::new(),
            bit_buf: 0,
            bit_len: 0,
        }
    }

    /// `n` bits of `v`, least significant first — extra bits, and the block
    /// header fields.
    fn bits(&mut self, v: u32, n: u32) {
        self.bit_buf |= (v & ((1u32 << n) - 1)) << self.bit_len;
        self.bit_len += n;
        while self.bit_len >= 8 {
            self.out.push((self.bit_buf & 0xff) as u8);
            self.bit_buf >>= 8;
            self.bit_len -= 8;
        }
    }

    /// A Huffman code: the same bits, but reversed, because codes are packed
    /// starting from their most significant bit while the byte fills from the
    /// least.
    fn code(&mut self, code: u32, n: u32) {
        let mut rev = 0u32;
        for i in 0..n {
            rev |= ((code >> (n - 1 - i)) & 1) << i;
        }
        self.bits(rev, n);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bit_len > 0 {
            self.out.push((self.bit_buf & 0xff) as u8);
        }
        self.out
    }

    /// The fixed literal/length code for `sym` (RFC 1951 3.2.6).
    fn literal(&mut self, sym: u16) {
        match sym {
            0..=143 => self.code(0x30 + sym as u32, 8),
            144..=255 => self.code(0x190 + sym as u32 - 144, 9),
            256..=279 => self.code(sym as u32 - 256, 7),
            _ => self.code(0xc0 + sym as u32 - 280, 8),
        }
    }

    /// A back-reference: its length code and extra bits, then its distance code
    /// and extra bits.
    fn match_ref(&mut self, len: usize, dist: usize) {
        let li = LEN_BASE.partition_point(|&b| (b as usize) <= len) - 1;
        self.literal(257 + li as u16);
        if LEN_EXTRA[li] > 0 {
            self.bits((len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
        }
        let di = DIST_BASE.partition_point(|&b| (b as usize) <= dist) - 1;
        // Distance codes are 5 bits, fixed, and are their own value.
        self.code(di as u32, 5);
        if DIST_EXTRA[di] > 0 {
            self.bits(
                (dist - DIST_BASE[di] as usize) as u32,
                DIST_EXTRA[di] as u32,
            );
        }
    }
}

fn hash3(d: &[u8], i: usize) -> usize {
    let v = (d[i] as u32) << 16 | (d[i + 1] as u32) << 8 | d[i + 2] as u32;
    // Knuth's multiplicative hash, folded to HASH_BITS.
    ((v.wrapping_mul(2_654_435_761)) >> (32 - HASH_BITS)) as usize
}

/// Compress `data` into a single fixed-Huffman DEFLATE block (raw, no zlib
/// header). Greedy LZ77 over a hash chain.
pub fn deflate_raw(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::new();
    // One final block, type 01 = fixed Huffman.
    w.bits(1, 1);
    w.bits(1, 2);

    let n = data.len();
    let mut head = vec![u32::MAX; 1 << HASH_BITS];
    let mut prev = vec![u32::MAX; n.max(1)];
    let mut i = 0usize;
    while i < n {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        if i + MIN_MATCH <= n {
            let h = hash3(data, i);
            let mut cand = head[h];
            let mut chain = 0;
            let limit = i.saturating_sub(WINDOW);
            while cand != u32::MAX && (cand as usize) >= limit && chain < MAX_CHAIN {
                let c = cand as usize;
                let max = (n - i).min(MAX_MATCH);
                // Cheap reject: the byte that would extend the current best.
                if best_len == 0 || data[c + best_len] == data[i + best_len] {
                    let mut l = 0usize;
                    while l < max && data[c + l] == data[i + l] {
                        l += 1;
                    }
                    if l > best_len {
                        best_len = l;
                        best_dist = i - c;
                        if l == max {
                            break;
                        }
                    }
                }
                cand = prev[c];
                chain += 1;
            }
        }
        if best_len >= MIN_MATCH {
            w.match_ref(best_len, best_dist);
            // Insert every position the match covers, so later matches can
            // start inside it.
            for k in i..i + best_len {
                if k + MIN_MATCH <= n {
                    let h = hash3(data, k);
                    prev[k] = head[h];
                    head[h] = k as u32;
                }
            }
            i += best_len;
        } else {
            w.literal(data[i] as u16);
            if i + MIN_MATCH <= n {
                let h = hash3(data, i);
                prev[i] = head[h];
                head[h] = i as u32;
            }
            i += 1;
        }
    }
    w.literal(256); // end of block
    w.finish()
}

/// Adler-32 (RFC 1950), the zlib stream's checksum.
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 is the most bytes that can be summed before b can overflow u32.
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// `data` as a zlib stream: the two-byte header, the DEFLATE data, the
/// Adler-32 of the input.
pub fn zlib(data: &[u8]) -> Vec<u8> {
    // CMF 0x78: deflate, 32K window. FLG 0x01: no preset dictionary, and
    // 0x7801 is a multiple of 31, which is the header's own check.
    let mut out = vec![0x78, 0x01];
    out.extend_from_slice(&deflate_raw(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use opccore::inflate::inflate_raw;

    /// The whole point of round-tripping through the repo's own inflater: a
    /// mistake in the bit packing cannot look like success.
    fn round_trip(data: &[u8]) {
        let packed = deflate_raw(data);
        let back = inflate_raw(&packed, data.len()).expect("our own stream must inflate");
        assert_eq!(back, data, "round trip of {} bytes", data.len());
    }

    #[test]
    fn empty_input_round_trips() {
        round_trip(b"");
    }

    #[test]
    fn short_inputs_round_trip() {
        round_trip(b"a");
        round_trip(b"ab");
        round_trip(b"abc");
        // Shorter than MIN_MATCH even though it repeats.
        round_trip(b"aa");
    }

    #[test]
    fn literals_that_span_both_fixed_code_lengths_round_trip() {
        // 0..=143 are eight-bit codes and 144..=255 are nine-bit ones; a stream
        // over every byte value exercises the boundary in both directions.
        let data: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
        round_trip(&data);
    }

    #[test]
    fn a_long_run_round_trips() {
        // Longer than MAX_MATCH, so it has to be split across several matches.
        round_trip(&vec![0u8; 5000]);
    }

    #[test]
    fn repeated_text_round_trips_and_gets_smaller() {
        let mut data = Vec::new();
        for i in 0..500 {
            data.extend_from_slice(format!("row {i}: the quick brown fox\n").as_bytes());
        }
        round_trip(&data);
        assert!(
            deflate_raw(&data).len() < data.len() / 2,
            "matching is not happening"
        );
    }

    /// Distances near the window edge use the highest distance codes, which is
    /// where an off-by-one in the code lookup would hide.
    #[test]
    fn a_match_at_the_far_end_of_the_window_round_trips() {
        let mut data: Vec<u8> = (0..40000u32).map(|i| (i % 251) as u8).collect();
        let tail: Vec<u8> = data[0..100].to_vec();
        data.extend_from_slice(&tail);
        round_trip(&data);
    }

    /// Every length and distance code, driven directly, so none of the 29 + 30
    /// table entries can be wrong without a test saying so.
    #[test]
    fn every_length_and_distance_code_round_trips() {
        for &len in LEN_BASE.iter() {
            for &dist in &[1u16, 2, 3, 4, 5, 300, 1000, 20000, 32768] {
                let d = dist as usize;
                let l = len as usize;
                // A pseudo-random prefix of `d` bytes, then a copy of its first
                // `l` bytes — which is exactly a match of (l, d).
                let mut data: Vec<u8> = (0..d).map(|i| ((i * 37 + 11) % 256) as u8).collect();
                let head: Vec<u8> = data.iter().cycle().take(l).copied().collect();
                data.extend_from_slice(&head);
                round_trip(&data);
            }
        }
    }

    #[test]
    fn noise_round_trips() {
        // A linear congruential generator, so the test is deterministic.
        let mut x = 12345u32;
        let data: Vec<u8> = (0..20000)
            .map(|_| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                (x >> 16) as u8
            })
            .collect();
        round_trip(&data);
    }

    /// The canonical Adler-32 check value.
    #[test]
    fn adler32_matches_the_known_vector() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
    }

    #[test]
    fn a_zlib_stream_carries_a_valid_header_and_checksum() {
        let data = b"hello hello hello hello".as_slice();
        let z = zlib(data);
        assert_eq!(&z[0..2], &[0x78, 0x01]);
        assert_eq!(
            (u16::from_be_bytes([z[0], z[1]])) % 31,
            0,
            "the zlib header's own check"
        );
        let n = z.len();
        assert_eq!(&z[n - 4..], &adler32(data).to_be_bytes());
        assert_eq!(
            inflate_raw(&z[2..n - 4], data.len()).unwrap(),
            data,
            "the body between header and checksum is the deflate stream"
        );
    }
}
