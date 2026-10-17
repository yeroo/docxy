//! The embedded-font half of the PDF writer: which face shows a character,
//! how wide it is, and the Identity-H text, subset font and ToUnicode objects
//! written for the faces a document used.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::*;
use crate::deflate::zlib;
use crate::pdf_font::{
    PdfFace, PdfFontProvider, subset_sfnt, subset_tag, to_unicode_cmap, w_array,
};

/// A face that shows a glyph only if its font file parses and subsets, found
/// out the first time one of its glyphs is wanted (the file is read then, as
/// embedding would read it anyway). Layout, the writer and the report then
/// agree: a face that cannot be embedded is absent, so its characters go to a
/// fallback, Courier, or a counted `.notdef`.
#[derive(Debug)]
struct CheckedFace {
    inner: Rc<dyn PdfFace>,
    usable: OnceCell<bool>,
}

impl CheckedFace {
    fn usable(&self) -> bool {
        *self.usable.get_or_init(|| {
            subset_sfnt(
                &self.inner.sfnt(),
                self.inner.face_index(),
                &BTreeSet::new(),
            )
            .is_some()
        })
    }
}

impl PdfFace for CheckedFace {
    fn id(&self) -> u64 {
        self.inner.id()
    }
    fn ps_name(&self) -> String {
        self.inner.ps_name()
    }
    fn units_per_em(&self) -> u16 {
        self.inner.units_per_em()
    }
    fn glyph(&self, ch: char) -> Option<u16> {
        self.inner.glyph(ch).filter(|_| self.usable())
    }
    fn advance(&self, gid: u16) -> u16 {
        self.inner.advance(gid)
    }
    fn ascent(&self) -> i16 {
        self.inner.ascent()
    }
    fn descent(&self) -> i16 {
        self.inner.descent()
    }
    fn sfnt(&self) -> Rc<[u8]> {
        self.inner.sfnt()
    }
    fn face_index(&self) -> u32 {
        self.inner.face_index()
    }
}

/// The host's provider with every face checked (see [`CheckedFace`]).
#[derive(Debug)]
pub(super) struct Checked {
    inner: Rc<dyn PdfFontProvider>,
    wrapped: RefCell<HashMap<u64, Rc<CheckedFace>>>,
    fallbacks: OnceCell<Vec<Rc<dyn PdfFace>>>,
}

impl Checked {
    pub(super) fn new(inner: Rc<dyn PdfFontProvider>) -> Checked {
        Checked {
            inner,
            wrapped: RefCell::default(),
            fallbacks: OnceCell::new(),
        }
    }

    fn wrap(&self, face: Rc<dyn PdfFace>) -> Rc<dyn PdfFace> {
        self.wrapped
            .borrow_mut()
            .entry(face.id())
            .or_insert_with(|| {
                Rc::new(CheckedFace {
                    inner: face,
                    usable: OnceCell::new(),
                })
            })
            .clone()
    }
}

impl PdfFontProvider for Checked {
    fn face(&self, family: &str, bold: bool, italic: bool) -> Option<Rc<dyn PdfFace>> {
        // The run's own face is about to be used, so it is checked now; the
        // fallbacks are checked when one of them is first needed.
        let face = self.wrap(self.inner.face(family, bold, italic)?);
        let checked = self.wrapped.borrow()[&face.id()].clone();
        checked.usable().then_some(face)
    }

    fn fallbacks(&self) -> &[Rc<dyn PdfFace>] {
        self.fallbacks.get_or_init(|| {
            self.inner
                .fallbacks()
                .iter()
                .map(|f| self.wrap(f.clone()))
                .collect()
        })
    }
}

/// What shows one character.
pub(super) enum Glyph {
    /// A glyph of an embedded face.
    Face(Rc<dyn PdfFace>, u16),
    /// The built-in Courier (WinAnsi) font, for what no face covers but
    /// WinAnsi does.
    Legacy,
    /// Nothing covers it: `.notdef` of the run's own face, or `?` without one.
    Missing(Option<Rc<dyn PdfFace>>),
}

fn winansi_covers(ch: char) -> bool {
    ch == '?' || winansi(ch) != b'?'
}

/// The run's own face, else a fallback face, else Courier (what WinAnsi
/// covers), else `.notdef` of the own face.
pub(super) fn resolve(p: &dyn PdfFontProvider, family: Option<&str>, font: u8, ch: char) -> Glyph {
    let own = p.face(family.unwrap_or(""), font & 1 != 0, font & 2 != 0);
    if let Some(face) = &own
        && let Some(gid) = face.glyph(ch)
    {
        return Glyph::Face(face.clone(), gid);
    }
    for face in p.fallbacks() {
        if let Some(gid) = face.glyph(ch) {
            return Glyph::Face(face.clone(), gid);
        }
    }
    if winansi_covers(ch) {
        return Glyph::Legacy;
    }
    Glyph::Missing(own)
}

/// The advance of `ch` in points.
pub(super) fn advance(
    p: &dyn PdfFontProvider,
    family: Option<&str>,
    font: u8,
    ch: char,
    size: f32,
) -> f32 {
    let of = |face: &Rc<dyn PdfFace>, gid: u16| {
        face.advance(gid) as f32 * size / face.units_per_em().max(1) as f32
    };
    match resolve(p, family, font, ch) {
        Glyph::Face(face, gid) => of(&face, gid),
        Glyph::Missing(Some(face)) => of(&face, 0),
        Glyph::Legacy | Glyph::Missing(None) => 0.6 * size,
    }
}

/// One face as used by the document so far.
struct FaceUse {
    face: Rc<dyn PdfFace>,
    gids: BTreeSet<u16>,
    text: BTreeMap<u16, Vec<char>>,
}

/// The faces the pages used, in order of first use, and what could not be
/// shown.
#[derive(Default)]
pub(super) struct Book {
    faces: Vec<FaceUse>,
    by_id: HashMap<u64, usize>,
    pub(super) missing: usize,
}

impl Book {
    pub(super) fn used(&self) -> bool {
        !self.faces.is_empty()
    }

    fn slot(&mut self, face: &Rc<dyn PdfFace>) -> usize {
        let next = self.faces.len();
        let slot = *self.by_id.entry(face.id()).or_insert(next);
        if slot == next {
            self.faces.push(FaceUse {
                face: face.clone(),
                gids: BTreeSet::new(),
                text: BTreeMap::new(),
            });
        }
        slot
    }
}

/// Text to draw: `chars` in logical order, its left edge at `left`. A `back`
/// run is a right-to-left stretch laid out right to left: its first char is
/// the rightmost.
pub(super) struct Text<'a> {
    pub chars: &'a [char],
    pub family: Option<&'a str>,
    pub font: u8,
    pub size: f32,
    pub left: f32,
    pub y: f32,
    pub back: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum Class {
    Face(usize),
    Legacy,
}

/// Write the show-text operators for `t` (each stretch in one font in its own
/// BT/ET) and record the glyphs used.
pub(super) fn draw(out: &mut Vec<u8>, book: &mut Book, p: &dyn PdfFontProvider, t: &Text) {
    struct Piece {
        class: Class,
        gid: u16,
        ch: char,
        w: f32,
    }
    let mut pieces: Vec<Piece> = Vec::with_capacity(t.chars.len());
    for &ch in t.chars {
        let w = advance(p, t.family, t.font, ch, t.size);
        let (class, gid) = match resolve(p, t.family, t.font, ch) {
            Glyph::Face(face, gid) => (Class::Face(book.slot(&face)), gid),
            Glyph::Missing(Some(face)) => {
                book.missing += 1;
                (Class::Face(book.slot(&face)), 0)
            }
            Glyph::Missing(None) => {
                book.missing += 1;
                (Class::Legacy, 0)
            }
            Glyph::Legacy => (Class::Legacy, 0),
        };
        if let Class::Face(slot) = class {
            let used = &mut book.faces[slot];
            used.gids.insert(gid);
            if gid != 0 {
                // One ToUnicode entry per glyph: keep the plainest character,
                // so U+0020 and U+00A0 sharing a glyph extract as a space.
                let kept = used.text.entry(gid).or_insert_with(|| vec![ch]);
                if ch.is_ascii() && !kept.iter().all(char::is_ascii) {
                    *kept = vec![ch];
                }
            }
        }
        pieces.push(Piece { class, gid, ch, w });
    }
    let total: f32 = pieces.iter().map(|p| p.w).sum();
    // Left edge of each piece.
    let mut xs = Vec::with_capacity(pieces.len());
    let mut run = 0.0;
    for piece in &pieces {
        run += piece.w;
        xs.push(if t.back {
            t.left + total - run
        } else {
            t.left + (run - piece.w)
        });
    }
    let mut i = 0;
    while i < pieces.len() {
        let mut j = i + 1;
        while j < pieces.len() && pieces[j].class == pieces[i].class {
            j += 1;
        }
        let group = &pieces[i..j];
        let tf = match group[0].class {
            Class::Face(slot) => format!("/C{slot}"),
            Class::Legacy => format!("/F{}", t.font),
        };
        out.extend(format!("BT {tf} {:.2} Tf {:.2} {:.2} Td ", t.size, xs[i], t.y).as_bytes());
        let shown = |piece: &Piece| -> Vec<u8> {
            match piece.class {
                Class::Face(_) => format!("<{:04X}>", piece.gid).into_bytes(),
                Class::Legacy => {
                    let mut s = vec![b'('];
                    s.extend(pdf_string_body(&piece.ch.to_string()));
                    s.push(b')');
                    s
                }
            }
        };
        if t.back && group.len() > 1 {
            out.push(b'[');
            for (k, piece) in group.iter().enumerate() {
                out.extend(shown(piece));
                if let Some(next) = group.get(k + 1) {
                    // Back from the end of this glyph to the start of the
                    // next, which lies to its left.
                    let n = (piece.w + next.w) * 1000.0 / t.size;
                    out.extend(format!("{n:.1}").as_bytes());
                }
            }
            out.extend(b"] TJ ET\n");
        } else if matches!(group[0].class, Class::Face(_)) {
            out.push(b'<');
            for piece in group {
                out.extend(format!("{:04X}", piece.gid).as_bytes());
            }
            out.extend(b"> Tj ET\n");
        } else {
            out.push(b'(');
            let text: String = group.iter().map(|p| p.ch).collect();
            out.extend(pdf_string_body(&text));
            out.extend(b") Tj ET\n");
        }
        i = j;
    }
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let z = zlib(data);
    [
        format!(
            "<< {dict}/Length {} /Filter /FlateDecode >>\nstream\n",
            z.len()
        )
        .into_bytes(),
        z,
        b"\nendstream".to_vec(),
    ]
    .concat()
}

/// Number of objects `objects` writes per face.
pub(super) const OBJS_PER_FACE: usize = 5;

fn pdf_name(ps: &str) -> String {
    let name: String = ps
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if name.is_empty() { "Font".into() } else { name }
}

impl Book {
    /// `(resource name, object id)` of every face's Type0 font, given the id
    /// of the first face object.
    pub(super) fn resources(&self, first_id: usize) -> Vec<(String, usize)> {
        (0..self.faces.len())
            .map(|k| (format!("C{k}"), first_id + k * OBJS_PER_FACE))
            .collect()
    }

    /// The objects of every face, `OBJS_PER_FACE` each, the first of which is
    /// the Type0 font; `first_id` is the id the first object will get.
    pub(super) fn objects(&self, first_id: usize) -> Vec<Vec<u8>> {
        let mut objs = Vec::new();
        for (k, used) in self.faces.iter().enumerate() {
            let id = first_id + k * OBJS_PER_FACE;
            let face = &used.face;
            let upem = face.units_per_em().max(1) as f32;
            let to1000 = |v: f32| (v * 1000.0 / upem).round() as i32;
            let name = format!(
                "{}+{}",
                subset_tag(face.id(), &used.gids),
                pdf_name(&face.ps_name())
            );
            let sfnt = face.sfnt();
            // Never embed what cannot be subset unless it is a whole plain font
            // (a collection, or an empty read, would leave the glyphs unshown).
            let font_file = subset_sfnt(&sfnt, face.face_index(), &used.gids).or_else(|| {
                let plain = matches!(sfnt.get(..4), Some([0, 1, 0, 0] | b"true"));
                (plain && face.face_index() == 0).then(|| sfnt.to_vec())
            });
            let widths: Vec<(u16, u32)> = used
                .gids
                .iter()
                .map(|&g| (g, to1000(face.advance(g) as f32).max(0) as u32))
                .collect();
            let cmap_entries: Vec<(u16, Vec<char>)> =
                used.text.iter().map(|(g, t)| (*g, t.clone())).collect();
            objs.push(
                format!(
                    "<< /Type /Font /Subtype /Type0 /BaseFont /{name} /Encoding /Identity-H /DescendantFonts [{} 0 R] /ToUnicode {} 0 R >>",
                    id + 1,
                    id + 4
                )
                .into_bytes(),
            );
            objs.push(
                format!(
                    "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{name} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {} 0 R /DW 1000 /W {} /CIDToGIDMap /Identity >>",
                    id + 2,
                    w_array(&widths)
                )
                .into_bytes(),
            );
            let (asc, desc) = (to1000(face.ascent() as f32), to1000(face.descent() as f32));
            objs.push(
                format!(
                    "<< /Type /FontDescriptor /FontName /{name} /Flags 4 /FontBBox [0 {desc} 1000 {asc}] /ItalicAngle 0 /Ascent {asc} /Descent {desc} /CapHeight {} /StemV 80{} >>",
                    asc * 7 / 10,
                    if font_file.is_some() {
                        format!(" /FontFile2 {} 0 R", id + 3)
                    } else {
                        String::new()
                    }
                )
                .into_bytes(),
            );
            objs.push(match &font_file {
                Some(file) => stream(&format!("/Length1 {} ", file.len()), file),
                None => b"null".to_vec(),
            });
            objs.push(stream("", to_unicode_cmap(&cmap_entries).as_bytes()));
        }
        objs
    }
}
