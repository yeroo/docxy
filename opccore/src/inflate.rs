//! DEFLATE decoder (RFC 1951), from scratch — no zlib. Canonical-Huffman
//! bit-at-a-time decode in the style of Mark Adler's puff.
//!
//! Ported from rust365 (`src/inflate.rs`), unchanged logic, plus unit tests.

const MAX_BITS: usize = 15;
const MAX_LCODES: usize = 286;
const MAX_DCODES: usize = 30;
const MAX_CODES: usize = MAX_LCODES + MAX_DCODES;
const FIX_LCODES: usize = 288;

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bitbuf: u32,
    bitcnt: i32,
    error: bool,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            bitbuf: 0,
            bitcnt: 0,
            error: false,
        }
    }
    fn bits(&mut self, need: i32) -> i32 {
        let mut val = self.bitbuf;
        while self.bitcnt < need {
            if self.pos >= self.data.len() {
                self.error = true;
                return 0;
            }
            val |= (self.data[self.pos] as u32) << self.bitcnt;
            self.pos += 1;
            self.bitcnt += 8;
        }
        self.bitbuf = val >> need;
        self.bitcnt -= need;
        (val & ((1u32 << need) - 1)) as i32
    }
}

struct Huffman {
    count: [i16; MAX_BITS + 1],
    symbol: [i16; FIX_LCODES],
}

impl Huffman {
    fn new() -> Self {
        Huffman {
            count: [0; MAX_BITS + 1],
            symbol: [0; FIX_LCODES],
        }
    }
}

/// Returns 0 for a complete code set, >0 incomplete, <0 over-subscribed.
fn construct(h: &mut Huffman, length: &[i16], n: usize) -> i32 {
    h.count = [0; MAX_BITS + 1];
    for &l in length.iter().take(n) {
        h.count[l as usize] += 1;
    }
    if h.count[0] as usize == n {
        return 0;
    }
    let mut left = 1i32;
    for len in 1..=MAX_BITS {
        left <<= 1;
        left -= h.count[len] as i32;
        if left < 0 {
            return left;
        }
    }
    let mut offs = [0i16; MAX_BITS + 2];
    offs[1] = 0;
    for len in 1..MAX_BITS {
        offs[len + 1] = offs[len] + h.count[len];
    }
    for sym in 0..n {
        if length[sym] != 0 {
            h.symbol[offs[length[sym] as usize] as usize] = sym as i16;
            offs[length[sym] as usize] += 1;
        }
    }
    left
}

fn decode(br: &mut BitReader, h: &Huffman) -> i32 {
    let mut code = 0i32;
    let mut first = 0i32;
    let mut index = 0i32;
    for len in 1..=MAX_BITS {
        code |= br.bits(1);
        if br.error {
            return -1;
        }
        let count = h.count[len] as i32;
        if code - count < first {
            return h.symbol[(index + (code - first)) as usize] as i32;
        }
        index += count;
        first += count;
        first <<= 1;
        code <<= 1;
    }
    -1
}

const LENS: [i16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [i16; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTS: [i16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [i16; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn codes(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    lencode: &Huffman,
    distcode: &Huffman,
    cap: usize,
) -> bool {
    loop {
        let sym = decode(br, lencode);
        if sym < 0 {
            return false;
        }
        if sym < 256 {
            out.push(sym as u8);
        } else if sym > 256 {
            let s = (sym - 257) as usize;
            if s >= 29 {
                return false;
            }
            let len = LENS[s] as i32 + br.bits(LEXT[s] as i32);
            let dsym = decode(br, distcode);
            if dsym < 0 || dsym as usize >= MAX_DCODES {
                return false;
            }
            let dist = DISTS[dsym as usize] as usize + br.bits(DEXT[dsym as usize] as i32) as usize;
            if br.error || dist > out.len() {
                return false;
            }
            let from = out.len() - dist;
            for i in 0..len as usize {
                out.push(out[from + i]);
            }
        }
        if cap != 0 && out.len() > cap {
            return false;
        }
        if sym == 256 {
            break;
        }
    }
    !br.error
}

fn fixed_tables() -> (Huffman, Huffman) {
    let mut lengths = [0i16; FIX_LCODES];
    let mut sym = 0;
    while sym < 144 {
        lengths[sym] = 8;
        sym += 1;
    }
    while sym < 256 {
        lengths[sym] = 9;
        sym += 1;
    }
    while sym < 280 {
        lengths[sym] = 7;
        sym += 1;
    }
    while sym < FIX_LCODES {
        lengths[sym] = 8;
        sym += 1;
    }
    let mut lencode = Huffman::new();
    construct(&mut lencode, &lengths, FIX_LCODES);
    for l in lengths.iter_mut().take(MAX_DCODES) {
        *l = 5;
    }
    let mut distcode = Huffman::new();
    construct(&mut distcode, &lengths, MAX_DCODES);
    (lencode, distcode)
}

fn stored_block(br: &mut BitReader, out: &mut Vec<u8>, cap: usize) -> bool {
    br.bitbuf = 0;
    br.bitcnt = 0;
    if br.pos + 4 > br.data.len() {
        return false;
    }
    let len = (br.data[br.pos] as usize) | ((br.data[br.pos + 1] as usize) << 8);
    let nlen = (br.data[br.pos + 2] as usize) | ((br.data[br.pos + 3] as usize) << 8);
    if (len ^ 0xFFFF) != nlen {
        return false;
    }
    br.pos += 4;
    if br.pos + len > br.data.len() {
        return false;
    }
    if cap != 0 && out.len() + len > cap {
        return false;
    }
    out.extend_from_slice(&br.data[br.pos..br.pos + len]);
    br.pos += len;
    true
}

fn dynamic_block(br: &mut BitReader, out: &mut Vec<u8>, cap: usize) -> bool {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let nlen = br.bits(5) as usize + 257;
    let ndist = br.bits(5) as usize + 1;
    let ncode = br.bits(4) as usize + 4;
    if br.error || nlen > MAX_LCODES || ndist > MAX_DCODES {
        return false;
    }
    let mut lengths = [0i16; MAX_CODES];
    for i in 0..ncode {
        lengths[ORDER[i]] = br.bits(3) as i16;
    }
    for i in ncode..19 {
        lengths[ORDER[i]] = 0;
    }
    if br.error {
        return false;
    }
    let mut lencode = Huffman::new();
    if construct(&mut lencode, &lengths, 19) != 0 {
        return false;
    }
    let mut index = 0usize;
    while index < nlen + ndist {
        let mut sym = decode(br, &lencode);
        if sym < 0 {
            return false;
        }
        if sym < 16 {
            lengths[index] = sym as i16;
            index += 1;
        } else {
            let mut repeat = 0i16;
            if sym == 16 {
                if index == 0 {
                    return false;
                }
                repeat = lengths[index - 1];
                sym = 3 + br.bits(2);
            } else if sym == 17 {
                sym = 3 + br.bits(3);
            } else {
                sym = 11 + br.bits(7);
            }
            if br.error || index + sym as usize > nlen + ndist {
                return false;
            }
            let mut s = sym;
            while s > 0 {
                lengths[index] = repeat;
                index += 1;
                s -= 1;
            }
        }
    }
    if lengths[256] == 0 {
        return false;
    }
    let err = construct(&mut lencode, &lengths, nlen);
    if err < 0 || (err > 0 && nlen as i32 - lencode.count[0] as i32 != 1) {
        return false;
    }
    let mut distcode = Huffman::new();
    let err = construct(&mut distcode, &lengths[nlen..], ndist);
    if err < 0 || (err > 0 && ndist as i32 - distcode.count[0] as i32 != 1) {
        return false;
    }
    codes(br, out, &lencode, &distcode, cap)
}

/// Decompresses a raw DEFLATE stream. `expected_size`, when non-zero, pre-reserves
/// and acts as a hard output cap. Returns None on malformed input.
pub fn inflate_raw(src: &[u8], expected_size: usize) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    if expected_size != 0 {
        out.reserve(expected_size.min(1usize << 26));
    }
    let cap = expected_size; // out starts empty, so cap == expected_size
    let mut br = BitReader::new(src);
    loop {
        let last = br.bits(1);
        let typ = br.bits(2);
        if br.error {
            return None;
        }
        let ok = match typ {
            0 => stored_block(&mut br, &mut out, cap),
            1 => {
                let (lencode, distcode) = fixed_tables();
                codes(&mut br, &mut out, &lencode, &distcode, cap)
            }
            2 => dynamic_block(&mut br, &mut out, cap),
            _ => return None,
        };
        if !ok {
            return None;
        }
        if last != 0 {
            break;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a single-block DEFLATE stream using the STORED (type 0) method,
    /// which is constructable by hand: BFINAL=1, BTYPE=00, then LEN/NLEN + data.
    fn stored_stream(payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0x01]; // bit0 = BFINAL(1), bits1-2 = BTYPE(00)
        let len = payload.len() as u16;
        v.extend_from_slice(&len.to_le_bytes());
        v.extend_from_slice(&(!len).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn stored_roundtrip() {
        let payload = b"Hello, docxcore!";
        let stream = stored_stream(payload);
        let out = inflate_raw(&stream, 0).expect("inflate stored");
        assert_eq!(out, payload);
    }

    #[test]
    fn stored_empty() {
        let stream = stored_stream(b"");
        let out = inflate_raw(&stream, 0).expect("inflate empty stored");
        assert!(out.is_empty());
    }

    #[test]
    fn stored_respects_expected_size_cap() {
        // expected_size acts as a hard cap; a too-small cap must reject.
        let stream = stored_stream(b"0123456789");
        assert!(inflate_raw(&stream, 4).is_none());
        // exact cap is fine.
        assert_eq!(inflate_raw(&stream, 10).unwrap(), b"0123456789");
    }

    #[test]
    fn bad_nlen_rejected() {
        let mut stream = stored_stream(b"abc");
        stream[3] ^= 0xFF; // corrupt the NLEN complement
        assert!(inflate_raw(&stream, 0).is_none());
    }

    #[test]
    fn truncated_input_rejected() {
        let stream = stored_stream(b"abcdef");
        // chop the payload short
        let truncated = &stream[..stream.len() - 2];
        assert!(inflate_raw(truncated, 0).is_none());
    }

    #[test]
    fn invalid_block_type_rejected() {
        // BFINAL=1, BTYPE=11 (reserved) -> byte 0b00000111
        assert!(inflate_raw(&[0x07], 0).is_none());
    }

    #[test]
    fn fixed_huffman_decodes() {
        // Smallest fixed-Huffman stream: just the end-of-block symbol (256),
        // whose fixed code is 7 zero bits. Bits are packed LSB-first per byte:
        //   bit0 = BFINAL(1), bit1..2 = BTYPE(01) -> low bit 1, high bit 0,
        //   bits3..7 = first 5 of the 7 zero EOB bits, byte1 bits0..1 = last 2.
        //   byte0 = 0b0000_0_0_1_1 = 0x03 ; byte1 = 0x00
        let out = inflate_raw(&[0x03, 0x00], 0).expect("fixed empty block");
        assert!(out.is_empty());
    }
}
