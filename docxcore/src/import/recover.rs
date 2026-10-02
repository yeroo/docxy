//! Word's Recover Text from Any File (#633): the text that can still be
//! read from a damaged `.docx`, or from any file at all.
//!
//! A `.docx` cut short (or with a broken central directory) still has its
//! local file headers in front of each part. [`recover_docx_text`] finds the
//! one for `word/document.xml`, decodes as much of it as is there (stored,
//! or DEFLATE through [`opccore::inflate::inflate_partial`], whatever size
//! the header declares) and reads its paragraphs with a lenient scan that
//! survives XML cut mid-tag. Text only, as Word's recovery gives: no
//! formatting, tables flattened to their paragraphs.
//!
//! [`recover_any_text`] is for everything else: runs of printable text, in
//! 8-bit (ASCII or UTF-8) and in UTF-16LE (how Word 97-2003 stores most
//! text), each a paragraph.

use super::{Budget, Builder, TOO_BIG};
use crate::model::{Document, ParProps, RunProps};

/// The text of a damaged Word package's main document part: `Ok(None)` when
/// no part can be found or it holds no text, `Err` when reading it would
/// cost more than an import may ([`TOO_BIG`]).
pub fn recover_docx_text(bytes: &[u8]) -> Result<Option<Document>, String> {
    recover_docx_text_within(bytes, &Budget::standard())
}

/// [`recover_docx_text`] against `budget`: the header scan is one pass over
/// the input (charged once), every part decoded is charged, and so is the
/// document built.
pub(crate) fn recover_docx_text_within(
    bytes: &[u8],
    budget: &Budget,
) -> Result<Option<Document>, String> {
    if !budget.scanned(bytes.len()) {
        return Err(TOO_BIG.into());
    }
    let xml = document_xml(bytes, budget);
    if budget.exhausted() {
        return Err(TOO_BIG.into());
    }
    let Some(xml) = xml else {
        return Ok(None);
    };
    let doc = paragraphs_of(&String::from_utf8_lossy(&xml), budget);
    if budget.exhausted() {
        return Err(TOO_BIG.into());
    }
    Ok((paragraph_count(&doc) > 0).then_some(doc))
}

/// Printable text runs of any file as paragraphs: 8-bit runs of at least
/// four characters and UTF-16LE runs of at least four, in file order. One
/// empty paragraph when nothing is readable; `Err` past the budget.
pub fn recover_any_text(bytes: &[u8]) -> Result<Document, String> {
    recover_any_text_within(bytes, &Budget::standard())
}

pub(crate) fn recover_any_text_within(bytes: &[u8], budget: &Budget) -> Result<Document, String> {
    // Three passes: 8-bit, and UTF-16 at both alignments.
    if !budget.work(3 * (bytes.len() / 16 + 1)) {
        return Err(TOO_BIG.into());
    }
    let mut runs = text_runs_8bit(bytes);
    runs.extend(text_runs_utf16(bytes));
    runs.sort_by_key(|(at, _)| *at);
    let plain = RunProps::default();
    let mut b = Builder::new(budget);
    for (_, text) in runs {
        if budget.exhausted() {
            return Err(TOO_BIG.into());
        }
        b.text(&text, &plain);
        b.end_para(ParProps::default(), false);
    }
    if b.is_empty() {
        b.end_para(ParProps::default(), false);
    }
    let doc = b.finish(ParProps::default());
    if budget.exhausted() {
        return Err(TOO_BIG.into());
    }
    Ok(doc)
}

/// How many paragraphs with text `doc` has: what the status line reports.
pub fn paragraph_count(doc: &Document) -> usize {
    super::paragraph_texts(doc)
        .iter()
        .filter(|t| !t.trim().is_empty())
        .count()
}

const LOCAL: &[u8] = b"PK\x03\x04";

/// Whether a part name is a main document part: `word/document.xml`, or
/// one named like it (`word/document2.xml`), never the glossary's
/// (`word/glossary/document.xml`, the building blocks).
fn main_part_name(name: &str) -> bool {
    name.strip_prefix("word/")
        .is_some_and(|f| !f.contains('/') && f.starts_with("document") && f.ends_with(".xml"))
}

/// The bytes of `word/document.xml` that can be read from a ZIP, cut short
/// or not; failing that, of a part named like it. Every part decoded is
/// charged to `budget`, and once a fallback is held no other one is decoded.
fn document_xml(bytes: &[u8], budget: &Budget) -> Option<Vec<u8>> {
    let mut fallback: Option<Vec<u8>> = None;
    let mut at = 0;
    while let Some(i) = super::find(&bytes[at..], LOCAL) {
        let h = at + i;
        at = h + 4;
        let Some(head) = bytes.get(h..h + 30) else {
            break;
        };
        let u16_at = |o: usize| usize::from(u16::from_le_bytes([head[o], head[o + 1]]));
        let u32_at = |o: usize| {
            u32::from_le_bytes([head[o], head[o + 1], head[o + 2], head[o + 3]]) as usize
        };
        let method = u16_at(8);
        let csize = u32_at(18);
        let name_len = u16_at(26);
        let extra_len = u16_at(28);
        let name_start = h + 30;
        let Some(name) = bytes.get(name_start..name_start + name_len) else {
            break;
        };
        let data_start = name_start + name_len + extra_len;
        if data_start > bytes.len() {
            break;
        }
        let name = String::from_utf8_lossy(name)
            .replace('\\', "/")
            .to_ascii_lowercase();
        if !main_part_name(&name) {
            continue;
        }
        let main = name == "word/document.xml";
        // Only the main part is worth decoding once a fallback is held:
        // many headers may point at one large compressed body.
        if !main && fallback.is_some() {
            continue;
        }
        let rest = &bytes[data_start..];
        let data = match method {
            0 => {
                // Stored: its declared size when it fits, else up to the next
                // header (a data-descriptor entry declares 0).
                let end = if csize > 0 && csize <= rest.len() {
                    csize
                } else {
                    super::find(rest, b"PK\x03\x04")
                        .or_else(|| super::find(rest, b"PK\x01\x02"))
                        .unwrap_or(rest.len())
                };
                if !budget.take_bytes(end) {
                    return None;
                }
                rest[..end].to_vec()
            }
            8 => {
                // Capped inside each block at what the budget has left.
                let room = budget.room();
                let out = opccore::inflate::inflate_partial(rest, room);
                if out.len() >= room || !budget.take_bytes(out.len()) {
                    budget.fail();
                    return None;
                }
                out
            }
            _ => continue,
        };
        if main {
            return Some(data);
        }
        fallback.get_or_insert(data);
    }
    fallback
}

/// The paragraphs of a (possibly cut) `document.xml`: `w:t` text, `w:tab`
/// and `w:br`/`w:cr`, one paragraph per `w:p`.
///
/// One forward pass (charged with the decoded bytes); the paragraphs are
/// charged by the builder.
fn paragraphs_of(xml: &str, budget: &Budget) -> Document {
    let plain = RunProps::default();
    let mut b = Builder::new(budget);
    let mut in_text = false;
    let mut in_run = false;
    let mut open_para = false;
    let mut pos = 0;
    while pos < xml.len() {
        let Some(lt) = xml[pos..].find('<').map(|i| pos + i) else {
            if in_text {
                // Cut inside the text: a reference cut short is not text.
                let mut tail = &xml[pos..];
                if let Some(amp) = tail.rfind('&').filter(|&a| !tail[a..].contains(';')) {
                    tail = &tail[..amp];
                }
                b.text(&unescape(tail), &plain);
            }
            break;
        };
        if in_text && lt > pos {
            b.text(&unescape(&xml[pos..lt]), &plain);
        }
        let Some(gt) = xml[lt..].find('>').map(|i| lt + i) else {
            break; // cut inside a tag
        };
        let tag = &xml[lt + 1..gt];
        pos = gt + 1;
        let close = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        match (name, close) {
            ("w:p", false) => {
                if open_para {
                    b.end_para(ParProps::default(), false);
                }
                open_para = !tag.ends_with('/');
                if !open_para {
                    b.end_para(ParProps::default(), false);
                }
            }
            ("w:p", true) => {
                if open_para {
                    b.end_para(ParProps::default(), false);
                    open_para = false;
                }
            }
            ("w:r", false) => in_run = !tag.ends_with('/'),
            ("w:r", true) => in_run = false,
            ("w:t", false) => in_text = !tag.ends_with('/'),
            ("w:t", true) => in_text = false,
            // Only a run's tab is text; `w:tab` in `w:pPr/w:tabs` is a tab
            // stop's definition.
            ("w:tab", false) if open_para && in_run => b.tab(&plain),
            ("w:br" | "w:cr", false) if open_para && in_run => b.line_break(&plain),
            _ => {}
        }
    }
    b.finish(ParProps::default())
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(semi) = rest.find(';').filter(|&j| j <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent.strip_prefix('#').and_then(|n| {
                match n.strip_prefix('x') {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => n.parse().ok(),
                }
                .and_then(char::from_u32)
            }),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Shortest run kept, in characters.
const MIN_RUN: usize = 4;

fn keep(at: usize, run: &mut String, out: &mut Vec<(usize, String)>) {
    let t = run.trim();
    if t.chars().count() >= MIN_RUN && t.chars().any(char::is_alphanumeric) {
        out.push((at, t.to_string()));
    }
    run.clear();
}

/// Printable ASCII / UTF-8 runs; a line break ends a run.
fn text_runs_8bit(bytes: &[u8]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut run = String::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (c, len) = if b < 0x80 {
            (char::from(b), 1)
        } else {
            let len = match b {
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                _ => 0,
            };
            match bytes
                .get(i..i + len.max(1))
                .and_then(|s| std::str::from_utf8(s).ok())
                .and_then(|s| s.chars().next())
            {
                Some(c) if len > 0 => (c, len),
                _ => ('\0', 1),
            }
        };
        let printable = (c == '\t' || c == ' ' || !c.is_control()) && c != '\0';
        if printable {
            if run.is_empty() {
                start = i;
            }
            run.push(c);
        } else {
            keep(start, &mut run, &mut out);
        }
        i += len;
    }
    keep(start, &mut run, &mut out);
    out
}

/// Printable UTF-16LE runs, read at both byte alignments; a line break ends
/// a run. Latin-1, Greek, Cyrillic, Hebrew and general punctuation count.
fn text_runs_utf16(bytes: &[u8]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for phase in 0..2 {
        let mut run = String::new();
        let mut start = phase;
        let mut i = phase;
        while i + 1 < bytes.len() {
            let unit = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
            let c = char::from_u32(u32::from(unit)).filter(|c| {
                (*c == '\t' || *c == ' ' || !c.is_control())
                    && !(0xD800..0xE000).contains(&unit)
                    && unit != 0xFFFD
                    && unit != 0xFEFF
            });
            // Two printable 8-bit bytes read as one CJK-looking unit, so
            // only these ranges count: 8-bit text never becomes UTF-16
            // noise, and the two passes never find the same text.
            let plausible = c.is_some()
                && (unit < 0x100
                    || (0x0370..0x0590).contains(&unit)
                    || (0x2000..0x2070).contains(&unit)
                    || unit == 0x20AC);
            if plausible {
                if run.is_empty() {
                    start = i;
                }
                run.push(c.expect("plausible implies a char"));
            } else {
                keep(start, &mut run, &mut out);
            }
            i += 2;
        }
        keep(start, &mut run, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::paragraph_texts;

    /// A ZIP local header for `name` holding `data` with `method`; `sizes`
    /// false writes 0 sizes and flag bit 3, as a streaming writer does.
    fn local(name: &str, method: u16, data: &[u8], sizes: bool) -> Vec<u8> {
        let mut v = LOCAL.to_vec();
        v.extend(20u16.to_le_bytes());
        v.extend((if sizes { 0u16 } else { 8 }).to_le_bytes());
        v.extend(method.to_le_bytes());
        v.extend([0, 0, 0, 0]); // time, date
        v.extend([0, 0, 0, 0]); // crc (unchecked)
        let n = if sizes { data.len() as u32 } else { 0 };
        v.extend(n.to_le_bytes());
        v.extend(n.to_le_bytes());
        v.extend((name.len() as u16).to_le_bytes());
        v.extend(0u16.to_le_bytes());
        v.extend(name.as_bytes());
        v.extend(data);
        v
    }

    /// Raw DEFLATE of `data` as stored blocks.
    fn deflate_stored(data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        let chunks: Vec<&[u8]> = data.chunks(1000).collect();
        for (i, c) in chunks.iter().enumerate() {
            v.push(u8::from(i + 1 == chunks.len()));
            let len = c.len() as u16;
            v.extend(len.to_le_bytes());
            v.extend((!len).to_le_bytes());
            v.extend_from_slice(c);
        }
        v
    }

    fn body(paras: &[&str]) -> String {
        let mut x = String::from("<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>");
        for p in paras {
            x.push_str("<w:p><w:pPr><w:pStyle w:val=\"Normal\"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space=\"preserve\">");
            x.push_str(&p.replace('&', "&amp;").replace('<', "&lt;"));
            x.push_str("</w:t></w:r></w:p>");
        }
        x.push_str("</w:body></w:document>");
        x
    }

    #[test]
    fn whole_and_cut_document_parts_read_as_paragraphs() {
        let paras: Vec<String> = (1..=40)
            .map(|i| format!("Paragraph {i} & more <text>"))
            .collect();
        let refs: Vec<&str> = paras.iter().map(String::as_str).collect();
        let xml = body(&refs);
        for (method, data) in [
            (0u16, xml.as_bytes().to_vec()),
            (8, deflate_stored(xml.as_bytes())),
        ] {
            for sizes in [true, false] {
                let mut zip = local("[Content_Types].xml", 0, b"<Types/>", true);
                zip.extend(local("word/document.xml", method, &data, sizes));
                zip.extend(local("word/styles.xml", 0, b"<w:styles/>", true));
                let whole = recover_docx_text(&zip).unwrap().unwrap();
                assert_eq!(
                    paragraph_texts(&whole),
                    paras,
                    "method {method} sizes {sizes}"
                );
                // Cut at half: a prefix comes back, its last paragraph
                // possibly partial.
                let cut = &zip[..zip.len() / 2];
                let got = paragraph_texts(&recover_docx_text(cut).unwrap().unwrap());
                assert!(got.len() >= 5 && got.len() < paras.len(), "{}", got.len());
                let (last, full) = got.split_last().unwrap();
                assert_eq!(full, &paras[..full.len()]);
                assert!(paras[full.len()].starts_with(last.as_str()));
            }
        }
    }

    #[test]
    fn tabs_breaks_and_deleted_text() {
        let xml = "<w:body><w:p><w:r><w:t>a</w:t><w:tab/><w:t>b</w:t><w:br/><w:t>c</w:t></w:r><w:del><w:r><w:delText>gone</w:delText></w:r></w:del></w:p><w:p/><w:p><w:r><w:instrText>PAGE</w:instrText><w:t>d&#x416;</w:t></w:r></w:p>";
        let zip = local("word/document.xml", 0, xml.as_bytes(), true);
        assert_eq!(
            paragraph_texts(&recover_docx_text(&zip).unwrap().unwrap()),
            ["a\tb\nc", "", "d\u{416}"]
        );
    }

    #[test]
    fn tab_stop_definitions_are_not_tabs() {
        let xml = "<w:body><w:p><w:pPr><w:tabs><w:tab w:val=\"left\" w:pos=\"720\"/><w:tab w:val=\"right\" w:pos=\"9360\"/></w:tabs></w:pPr><w:r><w:t>Chapter</w:t></w:r><w:r><w:tab/><w:t>7</w:t></w:r></w:p>";
        let zip = local("word/document.xml", 0, xml.as_bytes(), true);
        assert_eq!(
            paragraph_texts(&recover_docx_text(&zip).unwrap().unwrap()),
            ["Chapter\t7"]
        );
    }

    #[test]
    fn nothing_recoverable_is_none() {
        assert!(recover_docx_text(b"").unwrap().is_none());
        assert!(recover_docx_text(b"PK\x03\x04short").unwrap().is_none());
        let zip = local("word/styles.xml", 0, b"<w:styles/>", true);
        assert!(recover_docx_text(&zip).unwrap().is_none());
        let empty = local("word/document.xml", 0, b"<w:body><w:p/></w:body>", true);
        assert!(recover_docx_text(&empty).unwrap().is_none());
    }

    #[test]
    fn any_file_gives_its_text_runs() {
        let mut bytes = vec![0u8, 1, 2, 0xff];
        bytes.extend(b"Hello world\r\n");
        bytes.extend([0, 0, 7]);
        bytes.extend("Caf\u{e9} UTF-8".as_bytes());
        bytes.extend([0, 0x01]);
        bytes.extend(b"ab\x01\x01"); // too short, then a control
        for u in "Wide \u{416}\u{436} text".encode_utf16() {
            bytes.extend(u.to_le_bytes());
        }
        bytes.extend([0xff, 0xfe, 0x00, 0xd8]);
        let doc = recover_any_text(&bytes).unwrap();
        assert_eq!(
            paragraph_texts(&doc),
            ["Hello world", "Caf\u{e9} UTF-8", "Wide \u{416}\u{436} text"]
        );
        let none = recover_any_text(&[0, 1, 2, 3]).unwrap();
        assert_eq!(paragraph_texts(&none), [""]);
    }

    #[test]
    fn garbage_never_panics() {
        let xml = body(&["one", "two"]);
        let zip = local(
            "word/document.xml",
            8,
            &deflate_stored(xml.as_bytes()),
            false,
        );
        for n in 0..zip.len() {
            let _ = recover_docx_text(&zip[..n]);
            let _ = recover_any_text(&zip[..n]);
        }
        let _ = recover_docx_text(b"PK\x03\x04\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff");
    }

    /// FIX r2: `word/document2.xml` is a main part, the glossary's
    /// `word/glossary/document.xml` is not.
    #[test]
    fn main_part_names() {
        assert!(main_part_name("word/document.xml"));
        assert!(main_part_name("word/document2.xml"));
        assert!(!main_part_name("word/glossary/document.xml"));
        assert!(!main_part_name("word/xdocument.xml"));
        assert!(!main_part_name("word/styles.xml"));
        let glossary = body(&["Building block"]);
        let main = body(&["The document"]);
        let mut zip = local("word/glossary/document.xml", 0, glossary.as_bytes(), true);
        zip.extend(local("word/document2.xml", 0, main.as_bytes(), true));
        let got = recover_docx_text(&zip).unwrap().unwrap();
        assert_eq!(paragraph_texts(&got), ["The document"]);
    }

    /// FIX r2: many headers of main-like parts pointing (through their extra
    /// field) at one large body decode it once: once a fallback is held,
    /// another non-main part is not decoded at all.
    #[test]
    fn a_held_fallback_skips_decoding_the_others() {
        let paras: Vec<String> = (0..6000).map(|i| format!("Paragraph {i}")).collect();
        let refs: Vec<&str> = paras.iter().map(String::as_str).collect();
        let shared = deflate_stored(body(&refs).as_bytes());
        assert!(shared.len() > 200_000);
        // 100 headers, each pointing past the rest at the shared body.
        let n = 100;
        let header_len = |i: usize| 30 + format!("word/document{}.xml", i + 2).len();
        let total: usize = (0..n).map(header_len).sum();
        let mut zip = Vec::new();
        let mut at = 0;
        for i in 0..n {
            let name = format!("word/document{}.xml", i + 2);
            at += header_len(i);
            let extra = (total - at) as u16;
            zip.extend(LOCAL);
            zip.extend(20u16.to_le_bytes());
            zip.extend(8u16.to_le_bytes());
            zip.extend(8u16.to_le_bytes());
            zip.extend([0; 8]);
            zip.extend(0u32.to_le_bytes());
            zip.extend(0u32.to_le_bytes());
            zip.extend((name.len() as u16).to_le_bytes());
            zip.extend(extra.to_le_bytes());
            zip.extend(name.as_bytes());
        }
        zip.extend(&shared);
        // Decoding it once fits 4 MiB; decoding it for every header would not.
        let budget = Budget::new(4 << 20, crate::import::MAX_WORK);
        let got = recover_docx_text_within(&zip, &budget).unwrap().unwrap();
        assert_eq!(paragraph_texts(&got).len(), 6000);
    }

    #[test]
    fn a_bomb_in_the_main_part_fails_within_the_budget() {
        // A stored stream far larger than the room: Err, not 256 MiB.
        let big = vec![b'a'; 2 << 20];
        let zip = local("word/document.xml", 8, &deflate_stored(&big), false);
        let budget = Budget::new(1 << 20, crate::import::MAX_WORK);
        assert_eq!(
            recover_docx_text_within(&zip, &budget).unwrap_err(),
            TOO_BIG
        );
    }
}
