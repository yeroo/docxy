//! A std-only zlib compressor (RFC 1950/1951) for PDF `FlateDecode` streams.
//!
//! LZ77 with a hash-chain match finder and the fixed Huffman code, so the
//! output is deterministic. Data that does not shrink is stored instead.

const WINDOW: usize = 32768;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const MAX_CHAIN: usize = 64;
const HASH_BITS: usize = 15;

/// Length codes 257..=285: (base length, extra bits).
const LEN_TABLE: [(u16, u8); 29] = [
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 1),
    (13, 1),
    (15, 1),
    (17, 1),
    (19, 2),
    (23, 2),
    (27, 2),
    (31, 2),
    (35, 3),
    (43, 3),
    (51, 3),
    (59, 3),
    (67, 4),
    (83, 4),
    (99, 4),
    (115, 4),
    (131, 5),
    (163, 5),
    (195, 5),
    (227, 5),
    (258, 0),
];

/// Distance codes 0..=29: (base distance, extra bits).
const DIST_TABLE: [(u16, u8); 30] = [
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 1),
    (7, 1),
    (9, 2),
    (13, 2),
    (17, 3),
    (25, 3),
    (33, 4),
    (49, 4),
    (65, 5),
    (97, 5),
    (129, 6),
    (193, 6),
    (257, 7),
    (385, 7),
    (513, 8),
    (769, 8),
    (1025, 9),
    (1537, 9),
    (2049, 10),
    (3073, 10),
    (4097, 11),
    (6145, 11),
    (8193, 12),
    (12289, 12),
    (16385, 13),
    (24577, 13),
];

struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    /// Write `count` bits of `value`, least significant first.
    fn put(&mut self, value: u32, count: u32) {
        self.acc |= value << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Write a Huffman code (most significant bit first).
    fn code(&mut self, code: u32, len: u32) {
        let mut rev = 0;
        for i in 0..len {
            rev |= ((code >> i) & 1) << (len - 1 - i);
        }
        self.put(rev, len);
    }

    /// Write literal/length symbol `sym` with the fixed code.
    fn symbol(&mut self, sym: u32) {
        match sym {
            0..=143 => self.code(0x30 + sym, 8),
            144..=255 => self.code(0x190 + sym - 144, 9),
            256..=279 => self.code(sym - 256, 7),
            _ => self.code(0xC0 + sym - 280, 8),
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

fn hash(d: &[u8], i: usize) -> usize {
    let v = (d[i] as u32) << 16 | (d[i + 1] as u32) << 8 | d[i + 2] as u32;
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS as u32)) as usize
}

fn raw_fixed(data: &[u8]) -> Vec<u8> {
    let mut w = Bits {
        out: Vec::with_capacity(data.len() / 2 + 16),
        acc: 0,
        n: 0,
    };
    w.put(1, 1); // final block
    w.put(1, 2); // fixed Huffman
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; data.len()];
    let mut i = 0;
    let insert = |head: &mut Vec<usize>, prev: &mut Vec<usize>, p: usize| {
        if p + MIN_MATCH <= data.len() {
            let h = hash(data, p);
            prev[p] = head[h];
            head[h] = p;
        }
    };
    while i < data.len() {
        let mut best = (0usize, 0usize);
        if i + MIN_MATCH <= data.len() {
            let max = (data.len() - i).min(MAX_MATCH);
            let mut cand = head[hash(data, i)];
            let mut chain = 0;
            while cand != usize::MAX && i - cand <= WINDOW && chain < MAX_CHAIN {
                let mut l = 0;
                while l < max && data[cand + l] == data[i + l] {
                    l += 1;
                }
                if l > best.0 {
                    best = (l, i - cand);
                    if l == max {
                        break;
                    }
                }
                cand = prev[cand];
                chain += 1;
            }
        }
        if best.0 >= MIN_MATCH {
            let (len, dist) = best;
            let li = LEN_TABLE
                .iter()
                .rposition(|&(base, _)| base as usize <= len)
                .unwrap();
            w.symbol(257 + li as u32);
            let (base, extra) = LEN_TABLE[li];
            w.put((len - base as usize) as u32, extra as u32);
            let di = DIST_TABLE
                .iter()
                .rposition(|&(base, _)| base as usize <= dist)
                .unwrap();
            w.code(di as u32, 5);
            let (base, extra) = DIST_TABLE[di];
            w.put((dist - base as usize) as u32, extra as u32);
            for p in i..i + len {
                insert(&mut head, &mut prev, p);
            }
            i += len;
        } else {
            w.symbol(data[i] as u32);
            insert(&mut head, &mut prev, i);
            i += 1;
        }
    }
    w.symbol(256);
    w.finish()
}

fn raw_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65535 * 5 + 10);
    let mut chunks = data.chunks(65535).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(c) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let n = c.len() as u16;
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&(!n).to_le_bytes());
        out.extend_from_slice(c);
    }
    out
}

/// Raw deflate (RFC 1951): fixed-Huffman LZ77, or stored blocks when that is
/// not smaller.
pub fn deflate_raw(data: &[u8]) -> Vec<u8> {
    let packed = raw_fixed(data);
    let stored_len = data.len() + data.len().div_ceil(65535).max(1) * 5;
    if packed.len() <= stored_len {
        packed
    } else {
        raw_stored(data)
    }
}

/// A zlib stream (header, deflate, adler32), what a PDF `FlateDecode` reads.
pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x9C];
    out.extend(deflate_raw(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use opccore::inflate::inflate_raw;

    fn round_trip(data: &[u8]) {
        let z = zlib(data);
        assert_eq!(&z[..2], &[0x78, 0x9C]);
        let (body, tail) = z[2..].split_at(z.len() - 6);
        assert_eq!(tail, adler32(data).to_be_bytes());
        assert_eq!(inflate_raw(body, data.len()).as_deref(), Some(data));
        assert_eq!(z, zlib(data), "deterministic");
    }

    fn noise(n: usize) -> Vec<u8> {
        let mut x = 0x1234_5678u32;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 8) as u8
            })
            .collect()
    }

    #[test]
    fn empty_and_tiny() {
        round_trip(b"");
        round_trip(b"a");
        round_trip(b"ab");
    }

    #[test]
    fn repetitive_data_shrinks() {
        let data = b"the quick brown fox ".repeat(500);
        round_trip(&data);
        assert!(zlib(&data).len() < data.len() / 5);
        round_trip(&vec![0u8; 100_000]);
    }

    #[test]
    fn random_data_round_trips_and_stays_bounded() {
        let data = noise(200_000);
        round_trip(&data);
        assert!(zlib(&data).len() <= data.len() + data.len() / 65535 * 5 + 6 + 10);
    }

    #[test]
    fn long_distance_and_mixed() {
        let mut data = noise(40_000);
        let head = data[..300].to_vec();
        data.extend_from_slice(&head);
        data.extend(b"x".repeat(1000));
        round_trip(&data);
    }

    #[test]
    fn adler_known_vector() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }
}
