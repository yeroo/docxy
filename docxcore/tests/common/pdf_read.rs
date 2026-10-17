//! A small reader for the PDFs `to_pdf` writes: a structural check (xref,
//! stream lengths, references) and text extraction through ToUnicode, so the
//! tests do not depend on `qpdf` being installed.
#![allow(dead_code)]

use std::collections::BTreeMap;

pub struct Obj {
    pub dict: String,
    pub stream: Option<Vec<u8>>,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// Every object by id, split with the xref table. Panics on a broken file,
/// which is the point.
pub fn objects(pdf: &[u8]) -> BTreeMap<usize, Obj> {
    let start = rfind(pdf, b"startxref\n").expect("startxref") + 10;
    let end = find(pdf, b"\n", start).unwrap();
    let xref_pos: usize = std::str::from_utf8(&pdf[start..end])
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        pdf[xref_pos..].starts_with(b"xref\n0 "),
        "xref at its offset"
    );
    let head_end = find(pdf, b"\n", xref_pos + 5).unwrap();
    let count: usize = std::str::from_utf8(&pdf[xref_pos + 7..head_end])
        .unwrap()
        .parse()
        .unwrap();
    let mut offsets = Vec::new();
    for i in 1..count {
        let at = head_end + 1 + 20 * i;
        let entry = std::str::from_utf8(&pdf[at..at + 20]).unwrap();
        assert!(entry.ends_with(" n \n"), "entry {i}: {entry:?}");
        offsets.push(entry[..10].parse::<usize>().unwrap());
    }
    let mut out = BTreeMap::new();
    for (k, &off) in offsets.iter().enumerate() {
        let id = k + 1;
        let header = format!("{id} 0 obj\n");
        assert!(
            pdf[off..].starts_with(header.as_bytes()),
            "object {id} at its xref offset"
        );
        let stop = offsets.get(k + 1).copied().unwrap_or(xref_pos);
        let body = &pdf[off + header.len()..stop];
        let body = body.strip_suffix(b"\nendobj\n").expect("endobj");
        let obj = match find(body, b"\nstream\n", 0) {
            Some(s) => {
                let dict = String::from_utf8_lossy(&body[..s]).into_owned();
                let data = body[s + 8..]
                    .strip_suffix(b"\nendstream")
                    .expect("endstream");
                let len: usize = dict
                    .split("/Length ")
                    .nth(1)
                    .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
                    .and_then(|n| n.parse().ok())
                    .expect("/Length");
                assert_eq!(len, data.len(), "object {id}: /Length matches its stream");
                Obj {
                    dict,
                    stream: Some(data.to_vec()),
                }
            }
            None => Obj {
                dict: String::from_utf8_lossy(body).into_owned(),
                stream: None,
            },
        };
        out.insert(id, obj);
    }
    out
}

/// Every `N 0 R` in `dict`.
pub fn refs(dict: &str) -> Vec<usize> {
    let toks: Vec<&str> = dict
        .split(|c: char| c.is_whitespace() || "[]<>/".contains(c))
        .filter(|t| !t.is_empty())
        .collect();
    toks.windows(3)
        .filter(|w| w[2] == "R" && w[1] == "0")
        .filter_map(|w| w[0].parse().ok())
        .collect()
}

/// The stream of `obj`, inflated when it is Flate-compressed.
pub fn decoded(obj: &Obj) -> Vec<u8> {
    let data = obj.stream.as_ref().expect("a stream");
    if !obj.dict.contains("/FlateDecode") {
        return data.clone();
    }
    assert_eq!(&data[..2], &[0x78, 0x9C]);
    let body = &data[2..data.len() - 4];
    let out = opccore::inflate::inflate_raw(body, 64 << 20).expect("inflate");
    assert_eq!(
        data[data.len() - 4..],
        docxcore::deflate::adler32(&out).to_be_bytes(),
        "adler32"
    );
    out
}

/// Structural checks: the xref, stream lengths, and that every reference
/// resolves.
pub fn validate(pdf: &[u8]) -> BTreeMap<usize, Obj> {
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(pdf.ends_with(b"%%EOF\n"));
    let objs = objects(pdf);
    for (id, obj) in &objs {
        for r in refs(&obj.dict) {
            assert!(objs.contains_key(&r), "object {id} references missing {r}");
        }
        if obj.stream.is_some() {
            decoded(obj);
        }
    }
    objs
}

/// A font's ToUnicode: glyph id to text.
pub fn to_unicode(objs: &BTreeMap<usize, Obj>, font: &Obj) -> BTreeMap<u16, String> {
    let id = font
        .dict
        .split("/ToUnicode ")
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .expect("ToUnicode ref");
    let cmap = String::from_utf8(decoded(&objs[&id])).unwrap();
    let mut map = BTreeMap::new();
    for line in cmap.lines() {
        let parts: Vec<&str> = line
            .split(['<', '>'])
            .filter(|p| !p.trim().is_empty())
            .collect();
        if parts.len() == 2 && parts[0].len() == 4 && line.starts_with('<') {
            let cid = u16::from_str_radix(parts[0], 16).unwrap();
            let units: Vec<u16> = (0..parts[1].len() / 4)
                .map(|i| u16::from_str_radix(&parts[1][4 * i..4 * i + 4], 16).unwrap())
                .collect();
            map.insert(cid, String::from_utf16(&units).unwrap());
        }
    }
    map
}

/// One drawn run: where it starts and what it says.
#[derive(Debug, Clone)]
pub struct Shown {
    pub font: String,
    pub x: f32,
    pub y: f32,
    pub text: String,
    pub tj: bool,
}

/// Every show-text operation of each page's content, in stream order, with
/// the text read back through the font's ToUnicode.
pub fn shown(pdf: &[u8]) -> Vec<Vec<Shown>> {
    let objs = validate(pdf);
    let mut pages = Vec::new();
    for obj in objs.values() {
        if !obj.dict.contains("/Type /Page ") {
            continue;
        }
        let content_id: usize = obj
            .dict
            .split("/Contents ")
            .nth(1)
            .and_then(|r| r.split_whitespace().next())
            .unwrap()
            .parse()
            .unwrap();
        let mut fonts: BTreeMap<String, BTreeMap<u16, String>> = BTreeMap::new();
        let res = obj.dict.split("/Font <<").nth(1).unwrap();
        let res = &res[..res.find(">>").unwrap()];
        let toks: Vec<&str> = res.split_whitespace().collect();
        for w in toks.chunks(4) {
            if w.len() == 4 && w[0].starts_with("/C") {
                let id: usize = w[1].parse().unwrap();
                fonts.insert(w[0][1..].to_string(), to_unicode(&objs, &objs[&id]));
            }
        }
        let content = String::from_utf8_lossy(&decoded(&objs[&content_id])).into_owned();
        let mut out = Vec::new();
        let (mut font, mut x, mut y) = (String::new(), 0.0, 0.0);
        for line in content.lines() {
            let t: Vec<&str> = line.split_whitespace().collect();
            let Some(bt) = t.iter().position(|w| *w == "BT") else {
                continue;
            };
            let t = &t[bt + 1..];
            if let Some(i) = t.iter().position(|w| *w == "Tf") {
                font = t[i - 2].trim_start_matches('/').to_string();
            }
            if let Some(i) = t.iter().position(|w| *w == "Td") {
                x = t[i - 2].parse().unwrap();
                y = t[i - 1].parse().unwrap();
            }
            let body_at = t.iter().position(|w| *w == "Td").unwrap() + 1;
            let rest = t[body_at..].join(" ");
            let tj = rest.ends_with("TJ ET");
            let shown_part = rest
                .trim_end_matches("ET")
                .trim_end()
                .trim_end_matches("TJ")
                .trim_end_matches("Tj")
                .trim();
            let mut text = String::new();
            if let Some(map) = fonts.get(&font) {
                let mut s = shown_part;
                while let Some(open) = s.find('<') {
                    let close = s[open..].find('>').unwrap() + open;
                    let hex = &s[open + 1..close];
                    for k in 0..hex.len() / 4 {
                        let gid = u16::from_str_radix(&hex[4 * k..4 * k + 4], 16).unwrap();
                        text.push_str(map.get(&gid).map_or("\u{FFFD}", |s| s));
                    }
                    s = &s[close + 1..];
                }
            } else {
                let mut s = shown_part;
                while let Some(open) = s.find('(') {
                    let close = s.rfind(')').unwrap();
                    text.push_str(&s[open + 1..close]);
                    s = &s[close + 1..];
                }
            }
            out.push(Shown {
                font: font.clone(),
                x,
                y,
                text,
                tj,
            });
        }
        pages.push(out);
    }
    pages
}

/// All the text of the file in content order, whitespace removed.
pub fn text_of(pdf: &[u8]) -> String {
    shown(pdf)
        .iter()
        .flatten()
        .flat_map(|s| s.text.chars())
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// [`text_of`] for the body only: the header and footer sit outside
/// y 60..735 on a Letter page.
pub fn body_text(pdf: &[u8]) -> String {
    shown(pdf)
        .iter()
        .flatten()
        .filter(|s| s.y > 60.0 && s.y < 735.0)
        .flat_map(|s| s.text.chars())
        .filter(|c| !c.is_whitespace())
        .collect()
}
