//! Decode legacy Microsoft Equation Editor 3.0 (`Equation.3`) objects to inline
//! Unicode math text, so equations render as crisp body-sized text instead of a
//! scaled raster preview.
//!
//! Each object is an OLE2 compound file (CFB) holding an `Equation Native`
//! stream, which is an `EQNOLEFILEHDR` followed by an MTEF v3 byte stream. We
//! read the stream out of the compound file and walk the MTEF records, turning
//! characters (whose values are already Unicode) and the common templates
//! (sub/superscript, fraction, radical, fences) into a linear string.
//!
//! Coverage is the common case; anything we can't make sense of returns `None`
//! and the caller falls back to the rendered preview image.

use crate::mathbox::{MBox, bracket, grid, hcat, vstack};

/// Decode an `Equation.3` OLE object (`oleObjectN.bin` bytes) to Unicode text.
pub fn decode(ole_bin: &[u8]) -> Option<String> {
    let stream = cfb_read_stream(ole_bin, "Equation Native")?;
    decode_equation_native(&stream)
}

/// Strip the `EQNOLEFILEHDR` and decode the MTEF body.
fn decode_equation_native(stream: &[u8]) -> Option<String> {
    // EQNOLEFILEHDR.cbHdr (first WORD) is the header size; MTEF starts there.
    let cb = u16::from_le_bytes([*stream.first()?, *stream.get(1)?]) as usize;
    let mtef = stream.get(cb..)?;
    decode_mtef(mtef)
}

/// Decode an MTEF v3 byte stream to a linear Unicode string.
pub fn decode_mtef(mtef: &[u8]) -> Option<String> {
    // 5-byte header: version, platform, product, product version, subversion.
    if mtef.len() < 5 || mtef[0] != 3 {
        return None;
    }
    let mut p = Mtef {
        b: mtef,
        pos: 5,
        depth: 0,
    };
    let s = crate::mathbox::flatten(&p.list());
    (!s.is_empty()).then_some(s)
}

struct Mtef<'a> {
    b: &'a [u8],
    pos: usize,
    depth: usize,
}

// Tag option flags (high nibble of the tag byte).
const XF_NULL: u8 = 0x10; // LINE: empty, no END follows
const XF_RULER: u8 = 0x20; // LINE/PILE: a RULER record follows
const XF_LSPACE: u8 = 0x40; // LINE: line-spacing value follows
const XF_EMBELL: u8 = 0x20; // CHAR: embellishment list follows
const XF_LMOVE: u8 = 0x80; // any structure record: nudge offset follows

impl Mtef<'_> {
    fn u8(&mut self) -> u8 {
        let v = self.b.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        v
    }
    fn u16(&mut self) -> u16 {
        let v = u16::from_le_bytes([
            self.b.get(self.pos).copied().unwrap_or(0),
            self.b.get(self.pos + 1).copied().unwrap_or(0),
        ]);
        self.pos += 2;
        v
    }
    fn eof(&self) -> bool {
        self.pos >= self.b.len()
    }

    /// A nudge offset (present when `XF_LMOVE` is set): 2 bytes, or 6 when either
    /// component is out of `[-128, 127]` (signalled by the first two being 128).
    fn skip_nudge(&mut self) {
        let dx = self.u8();
        let dy = self.u8();
        if dx == 128 && dy == 128 {
            self.u16();
            self.u16();
        }
    }

    /// Parse an object list (records up to an END), laying its pieces out in a row.
    fn list(&mut self) -> MBox {
        let mut items: Vec<MBox> = Vec::new();
        // Guard against malformed input causing runaway recursion.
        if self.depth > 40 {
            return MBox::empty();
        }
        loop {
            if self.eof() {
                break;
            }
            let tag = self.u8();
            let typ = tag & 0x0f;
            let flags = tag & 0xf0;
            match typ {
                0 => break, // END
                1 => {
                    // LINE
                    if flags & XF_NULL != 0 {
                        continue; // empty line, no END
                    }
                    if flags & XF_LMOVE != 0 {
                        self.skip_nudge();
                    }
                    if flags & XF_LSPACE != 0 {
                        self.u8();
                    }
                    if flags & XF_RULER != 0 {
                        self.ruler();
                    }
                    items.push(self.list());
                }
                2 => {
                    // CHAR
                    if flags & XF_LMOVE != 0 {
                        self.skip_nudge();
                    }
                    let _typeface = self.u8();
                    let ch = self.u16();
                    if let Some(c) = char::from_u32(ch as u32) {
                        items.push(MBox::line(c.to_string()));
                    }
                    if flags & XF_EMBELL != 0 {
                        self.list(); // embellishment list, terminated by END
                    }
                }
                3 => {
                    // TMPL
                    if flags & XF_LMOVE != 0 {
                        self.skip_nudge();
                    }
                    let selector = self.u8();
                    let _variation = self.u8();
                    let _options = self.u8();
                    self.depth += 1;
                    let slots = self.slots();
                    self.depth -= 1;
                    items.push(format_template(selector, &slots));
                }
                4 => {
                    // PILE: a vertical stack of lines
                    if flags & XF_LMOVE != 0 {
                        self.skip_nudge();
                    }
                    self.u8(); // h-align
                    self.u8(); // v-align
                    if flags & XF_RULER != 0 {
                        self.ruler();
                    }
                    self.depth += 1;
                    let slots = self.slots();
                    self.depth -= 1;
                    let mid = slots.len() / 2;
                    items.push(vstack(&slots, mid));
                }
                5 => items.push(self.matrix(flags)), // MATRIX
                6 => {
                    // EMBELL (standalone): an accent; skip its type byte.
                    if flags & XF_LMOVE != 0 {
                        self.skip_nudge();
                    }
                    self.u8();
                }
                7 => self.ruler(),
                8 => {
                    // FONT: typeface, style, null-terminated name.
                    self.u8();
                    self.u8();
                    while !self.eof() && self.u8() != 0 {}
                }
                9 => self.skip_size(),
                10..=14 => {} // TYPESIZE shorthands: no payload
                _ => break,   // unknown — stop rather than desync
            }
        }
        hcat(&items)
    }

    /// A template/pile sub-object list: each LINE is one slot.
    fn slots(&mut self) -> Vec<MBox> {
        let mut slots = Vec::new();
        if self.depth > 40 {
            return slots;
        }
        loop {
            if self.eof() {
                break;
            }
            let tag = self.u8();
            let typ = tag & 0x0f;
            let flags = tag & 0xf0;
            match typ {
                0 => break, // END of the template
                1 => {
                    // LINE = one slot
                    if flags & XF_NULL != 0 {
                        slots.push(MBox::empty());
                        continue;
                    }
                    if flags & XF_LMOVE != 0 {
                        self.skip_nudge();
                    }
                    if flags & XF_LSPACE != 0 {
                        self.u8();
                    }
                    if flags & XF_RULER != 0 {
                        self.ruler();
                    }
                    slots.push(self.list());
                }
                10..=14 => {} // size shorthand between slots
                9 => self.skip_size(),
                7 => self.ruler(),
                // A non-LINE object directly in the list: treat as its own slot.
                _ => {
                    self.pos -= 1;
                    let one = self.one_object();
                    if !one.is_blank() {
                        slots.push(one);
                    }
                }
            }
        }
        slots
    }

    /// Read exactly one element: a LINE record's content (or a single object if
    /// the next record isn't a LINE). Used for matrix cells, where the element
    /// count is fixed and the cells are not terminated by a shared END.
    fn read_line(&mut self) -> MBox {
        if self.eof() {
            return MBox::empty();
        }
        let tag = self.u8();
        let typ = tag & 0x0f;
        let flags = tag & 0xf0;
        match typ {
            1 => {
                if flags & XF_NULL != 0 {
                    return MBox::empty();
                }
                if flags & XF_LMOVE != 0 {
                    self.skip_nudge();
                }
                if flags & XF_LSPACE != 0 {
                    self.u8();
                }
                if flags & XF_RULER != 0 {
                    self.ruler();
                }
                self.list()
            }
            0 => MBox::empty(), // unexpected early END
            _ => {
                self.pos -= 1;
                self.one_object()
            }
        }
    }

    /// Parse exactly one object (used when a template slot isn't wrapped in LINE).
    fn one_object(&mut self) -> MBox {
        if self.eof() {
            return MBox::empty();
        }
        let tag = self.u8();
        let typ = tag & 0x0f;
        let flags = tag & 0xf0;
        match typ {
            2 => {
                if flags & XF_LMOVE != 0 {
                    self.skip_nudge();
                }
                self.u8();
                let ch = self.u16();
                if flags & XF_EMBELL != 0 {
                    self.list();
                }
                char::from_u32(ch as u32)
                    .map(|c| MBox::line(c.to_string()))
                    .unwrap_or_else(MBox::empty)
            }
            3 => {
                if flags & XF_LMOVE != 0 {
                    self.skip_nudge();
                }
                let selector = self.u8();
                self.u8();
                self.u8();
                self.depth += 1;
                let slots = self.slots();
                self.depth -= 1;
                format_template(selector, &slots)
            }
            5 => self.matrix(flags),
            _ => MBox::empty(),
        }
    }

    fn ruler(&mut self) {
        let n = self.u8();
        for _ in 0..n {
            self.u8(); // stop type
            self.u16(); // offset
        }
    }

    /// MATRIX record: a `rows`×`cols` grid of element object-lists, laid out as a
    /// column-aligned grid (any surrounding bracket is supplied by the fence).
    fn matrix(&mut self, flags: u8) -> MBox {
        if flags & XF_LMOVE != 0 {
            self.skip_nudge();
        }
        let _valign = self.u8();
        let _h_just = self.u8();
        let _v_just = self.u8();
        let rows = self.u8() as usize;
        let cols = self.u8() as usize;
        // Row/column line-partition arrays: 2 bits per entry, each array
        // byte-aligned. (rows+1) row entries, (cols+1) col entries.
        let skip_parts = |s: &mut Self, n: usize| {
            for _ in 0..(2 * n).div_ceil(8) {
                s.u8();
            }
        };
        skip_parts(self, rows + 1);
        skip_parts(self, cols + 1);
        if rows == 0 || cols == 0 || rows * cols > 256 {
            return MBox::empty();
        }
        // The element lists are END-terminated and omit empty cells, so read
        // elements until the terminating END (capped at rows*cols) rather than a
        // fixed count — otherwise we run past the terminator and desync.
        // The cells are an END-terminated list of LINE records. Typesize/ruler/
        // size/font records can sit *between* cells (setting the size of the next
        // one); skip those so they don't get mistaken for empty cells and shift
        // the whole grid. Empty cells are NULL LINEs and are kept.
        // Read exactly rows*cols cells. Each cell is a LINE record; between cells
        // there can be bare END separators and typesize/ruler/size/font records.
        // Skip those (they're not cells) so the grid doesn't shift. Empty cells are
        // NULL LINEs (handled by read_line), not bare ENDs.
        self.depth += 1;
        let mut flat: Vec<MBox> = Vec::new();
        while !self.eof() && self.depth <= 40 && flat.len() < rows * cols {
            let tag = self.b[self.pos];
            match tag & 0x0f {
                0 => self.pos += 1,               // bare END: an inter-cell separator
                1 => flat.push(self.read_line()), // a cell (LINE / NULL LINE)
                7 => {
                    self.pos += 1;
                    self.ruler();
                }
                8 => {
                    self.pos += 1;
                    self.u8();
                    self.u8();
                    while !self.eof() && self.u8() != 0 {}
                }
                9 => {
                    self.pos += 1;
                    self.skip_size();
                }
                10..=14 => self.pos += 1, // typesize shorthand between cells
                _ => flat.push(self.read_line()),
            }
        }
        self.depth -= 1;
        let cells: Vec<Vec<MBox>> = (0..rows)
            .map(|r| {
                (0..cols)
                    .map(|c| flat.get(r * cols + c).cloned().unwrap_or_else(MBox::empty))
                    .collect()
            })
            .collect();
        grid(&cells)
    }

    /// SIZE record (type 9), whose length depends on the first following byte.
    fn skip_size(&mut self) {
        match self.u8() {
            100 => {
                self.u8();
                self.u16();
            }
            101 => {
                self.u16();
            }
            _ => {
                self.u8();
            }
        }
    }
}

/// Render a template from its selector and slot texts.
fn format_template(selector: u8, slots: &[MBox]) -> MBox {
    let b = |i: usize| slots.get(i).cloned().unwrap_or_else(MBox::empty);
    let s = |i: usize| slots.get(i).map(MBox::flat).unwrap_or_default();
    // Fences (selectors 0..=5) around an empty slot are MathType placeholders;
    // drop them so we don't emit stray "()"/"[]" after a real construct.
    if selector <= 5 && b(0).is_blank() {
        return MBox::empty();
    }
    match selector {
        // Fences that grow tall around a multi-row child (e.g. a matrix):
        // angle / paren / brace / bracket / determinant bar / double bar.
        0 => bracket(b(0), '⟨', '⟩'),
        1 => bracket(b(0), '(', ')'),
        2 => bracket(b(0), '{', '}'),
        3 => bracket(b(0), '[', ']'),
        4 => bracket(b(0), '|', '|'),
        5 => bracket(b(0), '‖', '‖'),
        // Radicals: square root (and nth root) — selectors vary; treat 9..=11 as roots.
        9..=11 => {
            if s(1).is_empty() {
                MBox::line(format!("√({})", s(0)))
            } else {
                MBox::line(format!("{}√({})", superscript(&s(1)), s(0)))
            }
        }
        // Fraction.
        14 => MBox::line(format!("({})/({})", s(0), s(1))),
        // Subscript/superscript: slots are [subscript, superscript].
        15 => MBox::line(format!("{}{}", subscript(&s(0)), superscript(&s(1)))),
        // Unknown structure: keep the content (laid out in a row).
        _ => hcat(slots),
    }
}

/// Map a string to Unicode superscript, or `^(..)` when a char has no superscript.
fn superscript(s: &str) -> String {
    map_script(s, super_char, '^')
}
/// Map a string to Unicode subscript, or `_(..)` when a char has no subscript.
fn subscript(s: &str) -> String {
    map_script(s, sub_char, '_')
}

fn map_script(s: &str, f: fn(char) -> Option<char>, marker: char) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mapped: Option<String> = s.chars().map(f).collect();
    match mapped {
        Some(m) => m,
        None => format!("{marker}({s})"),
    }
}

fn super_char(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' | '−' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'n' => 'ⁿ',
        'i' => 'ⁱ',
        _ => return None,
    })
}

fn sub_char(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' | '−' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'o' => 'ₒ',
        'x' => 'ₓ',
        'h' => 'ₕ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'p' => 'ₚ',
        's' => 'ₛ',
        't' => 'ₜ',
        'v' => 'ᵥ',
        'r' => 'ᵣ',
        'u' => 'ᵤ',
        'j' => 'ⱼ',
        _ => return None,
    })
}

// ---- minimal OLE2 / Compound File Binary reader ----

/// Read a named stream from a CFB (OLE2 compound file). Supports the FAT and the
/// mini-stream; the header DIFAT (109 entries) covers the tiny equation objects.
fn cfb_read_stream(data: &[u8], name: &str) -> Option<Vec<u8>> {
    const SIG: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    if data.len() < 512 || data[..8] != SIG {
        return None;
    }
    let rd_u16 = |o: usize| u16::from_le_bytes([data[o], data[o + 1]]);
    let rd_u32 = |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
    let sect_size = 1usize << rd_u16(0x1e);
    let mini_size = 1usize << rd_u16(0x20);
    let mini_cutoff = rd_u32(0x38) as usize;
    let dir_start = rd_u32(0x30);
    let minifat_start = rd_u32(0x3c);
    if sect_size == 0 || sect_size > 4096 {
        return None;
    }

    let sector = |n: u32| -> Option<&[u8]> {
        let off = 512 + (n as usize).checked_mul(sect_size)?;
        data.get(off..off.checked_add(sect_size)?)
    };
    // FAT from the header DIFAT (first 109 sector ids at offset 0x4c).
    let mut fat: Vec<u32> = Vec::new();
    for i in 0..109 {
        let s = rd_u32(0x4c + i * 4);
        if s == 0xFFFFFFFF {
            break;
        }
        let sec = sector(s)?;
        for c in sec.chunks_exact(4) {
            fat.push(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
        }
    }
    let chain = |start: u32| -> Vec<u32> {
        let mut out = Vec::new();
        let mut s = start;
        while (s as usize) < fat.len() && s < 0xFFFFFFFE {
            out.push(s);
            s = fat[s as usize];
            if out.len() > 1 << 16 {
                break;
            }
        }
        out
    };
    let read_chain = |start: u32| -> Vec<u8> {
        let mut out = Vec::new();
        for s in chain(start) {
            if let Some(sec) = sector(s) {
                out.extend_from_slice(sec);
            }
        }
        out
    };

    // Directory entries (128 bytes each).
    let dir = read_chain(dir_start);
    let mut root_start = 0u32;
    let mut root_size = 0u32;
    let mut want: Option<(u32, u32)> = None;
    for e in dir.chunks_exact(128) {
        let nlen = u16::from_le_bytes([e[0x40], e[0x41]]) as usize;
        if !(2..=64).contains(&nlen) {
            continue;
        }
        let entry_type = e[0x42];
        let nm: String = e[..nlen - 2]
            .chunks_exact(2)
            .map(|c| char::from_u32(u16::from_le_bytes([c[0], c[1]]) as u32).unwrap_or('\u{fffd}'))
            .collect();
        let start = u32::from_le_bytes([e[0x74], e[0x75], e[0x76], e[0x77]]);
        let size = u32::from_le_bytes([e[0x78], e[0x79], e[0x7a], e[0x7b]]);
        if entry_type == 5 {
            root_start = start; // root entry holds the mini-stream
            root_size = size;
        } else if entry_type == 2 && nm == name {
            want = Some((start, size));
        }
    }
    let (start, size) = want?;
    let size = size as usize;

    if size >= mini_cutoff {
        let mut bytes = read_chain(start);
        bytes.truncate(size);
        return Some(bytes);
    }

    // Small stream: stored in the mini-stream (the root entry's regular stream),
    // addressed by the mini-FAT in mini_size units.
    let mut ministream = read_chain(root_start);
    ministream.truncate(root_size as usize);
    let minifat_bytes = read_chain(minifat_start);
    let minifat: Vec<u32> = minifat_bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let mut out = Vec::new();
    let mut s = start;
    while (s as usize) < minifat.len() && s < 0xFFFFFFFE {
        let off = (s as usize) * mini_size;
        if let Some(chunk) = ministream.get(off..off + mini_size) {
            out.extend_from_slice(chunk);
        }
        s = minifat[s as usize];
        if out.len() > size + mini_size {
            break;
        }
    }
    out.truncate(size);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_subsuperscript_equation() {
        // The "Equation Native" MTEF for c (subscript v, superscript 2) − 4mk < 0.
        let stream = hex(
            "1c0000000200d4c0410000000000000050a414003cbc14000000000003010103\
             010a0112836300030f01000b01128376000011000a030f00000b110102883200\
             00000a028612220288340012836d0012836b0002863c00028830000000",
        );
        assert_eq!(
            decode_equation_native(&stream).as_deref(),
            Some("cᵥ²−4mk<0")
        );
    }

    #[test]
    fn rejects_non_mtef() {
        assert!(decode_mtef(b"not mtef").is_none());
        assert!(decode(b"not a compound file").is_none());
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }
}
