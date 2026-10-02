//! Insert › Cover Page (#652): the built-in cover designs, and finding,
//! reading and replacing the cover a document holds.
//!
//! Word marks a cover page as a block content control whose
//! `w:docPartGallery` is `Cover Pages`, at the start of the body. Its content
//! ends with a page break, so removing or replacing the control takes the
//! whole first page with it. Inside, inline content controls hold the
//! placeholders (`[Document title]`, …), named by `w:alias`; text typed into
//! one carries into the same placeholder of another design.
//!
//! The loader keeps a content control as two `Raw` boundaries around normally
//! parsed content (see `load::parse_sdt_block`), at block and inline level,
//! so a cover we insert is parsed from its XML by the body loader itself and
//! looks to the editor exactly as it will after a reload.

use std::collections::HashMap;

use crate::hf::{doc_part_gallery, is_sdt_close, is_sdt_open, matching_close};
use crate::model::{Block, Inline};

/// The `w:docPartGallery` value of a cover page.
pub const COVER_GALLERY: &str = "Cover Pages";

/// A placeholder a cover design holds, matched by its `w:alias`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Placeholder {
    Title,
    Subtitle,
    Author,
    Date,
    Company,
    Abstract,
}

impl Placeholder {
    /// The `w:alias` (and `w:tag`) we write.
    pub fn alias(self) -> &'static str {
        match self {
            Placeholder::Title => "Title",
            Placeholder::Subtitle => "Subtitle",
            Placeholder::Author => "Author",
            Placeholder::Date => "Date",
            Placeholder::Company => "Company",
            Placeholder::Abstract => "Abstract",
        }
    }

    /// The text an untouched placeholder shows.
    pub fn prompt(self) -> &'static str {
        match self {
            Placeholder::Title => "[Document title]",
            Placeholder::Subtitle => "[Document subtitle]",
            Placeholder::Author => "[Author name]",
            Placeholder::Date => "[Date]",
            Placeholder::Company => "[Company name]",
            Placeholder::Abstract => "[Abstract]",
        }
    }

    /// The placeholder an alias (ours or Word's) names, ignoring case.
    /// Word's cover designs call the date `Publish Date`.
    pub fn from_alias(alias: &str) -> Option<Placeholder> {
        let a = alias.trim().to_ascii_lowercase();
        Some(match a.as_str() {
            "title" => Placeholder::Title,
            "subtitle" => Placeholder::Subtitle,
            "author" => Placeholder::Author,
            "date" | "publish date" => Placeholder::Date,
            "company" => Placeholder::Company,
            "abstract" => Placeholder::Abstract,
            _ => return None,
        })
    }
}

/// One paragraph of a design: its `w:pPr` and `w:rPr` children (direct
/// formatting only, in schema order, so a document without a Title style
/// still shows the design) and the placeholder it holds, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverPara {
    pub ppr: &'static str,
    pub rpr: &'static str,
    pub field: Option<Placeholder>,
}

/// A design of the Cover Page gallery. Text only: the renderers draw no
/// shapes or pictures, so a design is made of paragraph formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverDesign {
    pub name: &'static str,
    pub paras: &'static [CoverPara],
}

const fn para(ppr: &'static str, rpr: &'static str, field: Placeholder) -> CoverPara {
    CoverPara {
        ppr,
        rpr,
        field: Some(field),
    }
}

/// The designs the gallery offers, in menu order.
pub const COVER_DESIGNS: [CoverDesign; 4] = [
    CoverDesign {
        name: "Plain",
        paras: &[
            para(
                "<w:spacing w:before=\"2880\" w:after=\"240\"/>",
                "<w:sz w:val=\"72\"/><w:szCs w:val=\"72\"/>",
                Placeholder::Title,
            ),
            para(
                "<w:spacing w:after=\"1440\"/>",
                "<w:color w:val=\"595959\"/><w:sz w:val=\"32\"/><w:szCs w:val=\"32\"/>",
                Placeholder::Subtitle,
            ),
            para(
                "<w:spacing w:after=\"0\"/>",
                "<w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>",
                Placeholder::Author,
            ),
            para(
                "<w:spacing w:after=\"0\"/>",
                "<w:color w:val=\"595959\"/><w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>",
                Placeholder::Date,
            ),
        ],
    },
    CoverDesign {
        name: "Centered",
        paras: &[
            para(
                "<w:spacing w:before=\"3600\" w:after=\"240\"/><w:jc w:val=\"center\"/>",
                "<w:b/><w:sz w:val=\"64\"/><w:szCs w:val=\"64\"/>",
                Placeholder::Title,
            ),
            para(
                "<w:spacing w:after=\"2160\"/><w:jc w:val=\"center\"/>",
                "<w:color w:val=\"595959\"/><w:sz w:val=\"32\"/><w:szCs w:val=\"32\"/>",
                Placeholder::Subtitle,
            ),
            para(
                "<w:spacing w:after=\"0\"/><w:jc w:val=\"center\"/>",
                "<w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>",
                Placeholder::Author,
            ),
            para(
                "<w:spacing w:after=\"0\"/><w:jc w:val=\"center\"/>",
                "<w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>",
                Placeholder::Company,
            ),
            para(
                "<w:spacing w:after=\"0\"/><w:jc w:val=\"center\"/>",
                "<w:color w:val=\"595959\"/><w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>",
                Placeholder::Date,
            ),
        ],
    },
    CoverDesign {
        name: "Title Band",
        paras: &[
            para(
                "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"1F3864\"/>\
                 <w:spacing w:before=\"2880\" w:after=\"0\"/>",
                "<w:b/><w:color w:val=\"FFFFFF\"/><w:sz w:val=\"64\"/><w:szCs w:val=\"64\"/>",
                Placeholder::Title,
            ),
            para(
                "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"1F3864\"/>\
                 <w:spacing w:after=\"480\"/>",
                "<w:color w:val=\"FFFFFF\"/><w:sz w:val=\"32\"/><w:szCs w:val=\"32\"/>",
                Placeholder::Subtitle,
            ),
            para(
                "<w:spacing w:after=\"1440\"/>",
                "<w:i/><w:color w:val=\"404040\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/>",
                Placeholder::Abstract,
            ),
            para(
                "<w:spacing w:after=\"0\"/>",
                "<w:b/><w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>",
                Placeholder::Author,
            ),
            para(
                "<w:spacing w:after=\"0\"/>",
                "<w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>",
                Placeholder::Company,
            ),
            para(
                "<w:spacing w:after=\"0\"/>",
                "<w:color w:val=\"595959\"/><w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>",
                Placeholder::Date,
            ),
        ],
    },
    CoverDesign {
        name: "Ruled",
        paras: &[
            para(
                "<w:pBdr><w:top w:val=\"single\" w:sz=\"18\" w:space=\"12\" w:color=\"1F3864\"/>\
                 <w:bottom w:val=\"single\" w:sz=\"18\" w:space=\"12\" w:color=\"1F3864\"/></w:pBdr>\
                 <w:spacing w:before=\"3600\" w:after=\"360\"/><w:jc w:val=\"center\"/>",
                "<w:caps/><w:color w:val=\"1F3864\"/><w:sz w:val=\"56\"/><w:szCs w:val=\"56\"/>",
                Placeholder::Title,
            ),
            para(
                "<w:spacing w:after=\"2880\"/><w:jc w:val=\"center\"/>",
                "<w:color w:val=\"595959\"/><w:sz w:val=\"28\"/><w:szCs w:val=\"28\"/>",
                Placeholder::Subtitle,
            ),
            para(
                "<w:spacing w:after=\"0\"/><w:jc w:val=\"center\"/>",
                "<w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>",
                Placeholder::Author,
            ),
            para(
                "<w:spacing w:after=\"0\"/><w:jc w:val=\"center\"/>",
                "<w:color w:val=\"595959\"/><w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>",
                Placeholder::Date,
            ),
        ],
    },
];

/// Text typed into a cover's placeholders, by placeholder.
pub type Typed = HashMap<Placeholder, String>;

fn esc(s: &str) -> String {
    crate::mermaid::xml_escape_text(s)
}

/// `text` as runs with `rpr`: a line break (`\n`) is a `w:br` run and a tab a
/// `w:tab` run, so text gathered from several paragraphs stays one inline.
fn runs_xml(text: &str, rpr: &str) -> String {
    let rpr = if rpr.is_empty() {
        String::new()
    } else {
        format!("<w:rPr>{rpr}</w:rPr>")
    };
    let mut out = String::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push_str(&format!("<w:r>{rpr}<w:br/></w:r>"));
        }
        for (k, piece) in line.split('\t').enumerate() {
            if k > 0 {
                out.push_str(&format!("<w:r>{rpr}<w:tab/></w:r>"));
            }
            if !piece.is_empty() {
                out.push_str(&format!(
                    "<w:r>{rpr}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
                    esc(piece)
                ));
            }
        }
    }
    out
}

/// The block XML of a cover in `design`: Word's `docPartObj` content control
/// (`Cover Pages`, unique) holding the design's paragraphs, each placeholder
/// an inline content control showing the `typed` text for it or its bracketed
/// prompt, and a last paragraph holding the page break. `ids` gives the
/// controls' `w:id`s, the cover's first; it needs one more than the design
/// has placeholders.
///
/// No `w:showingPlcHdr`: our editor never clears it, so Word would take text
/// typed here for a placeholder. No `w:dataBinding` to the document
/// properties either.
pub fn cover_sdt_xml(design: &CoverDesign, typed: &Typed, ids: &[i64]) -> String {
    let id = |k: usize| ids.get(k).copied().unwrap_or(k as i64 + 1);
    let mut body = String::new();
    for (k, p) in design.paras.iter().enumerate() {
        let mark = if p.rpr.is_empty() {
            String::new()
        } else {
            format!("<w:rPr>{}</w:rPr>", p.rpr)
        };
        body.push_str(&format!("<w:p><w:pPr>{}{mark}</w:pPr>", p.ppr));
        if let Some(field) = p.field {
            let text = typed.get(&field).map_or(field.prompt(), String::as_str);
            body.push_str(&format!(
                "<w:sdt><w:sdtPr>{mark}<w:alias w:val=\"{a}\"/><w:tag w:val=\"{a}\"/>\
                 <w:id w:val=\"{}\"/></w:sdtPr><w:sdtContent>{}</w:sdtContent></w:sdt>",
                id(k + 1),
                runs_xml(text, p.rpr),
                a = field.alias(),
            ));
        }
        body.push_str("</w:p>");
    }
    format!(
        "<w:sdt><w:sdtPr><w:id w:val=\"{}\"/><w:docPartObj>\
         <w:docPartGallery w:val=\"{COVER_GALLERY}\"/><w:docPartUnique/></w:docPartObj>\
         </w:sdtPr><w:sdtContent>{body}<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>\
         </w:sdtContent></w:sdt>",
        id(0)
    )
}

/// The blocks of body XML `xml`, as the body loader reads them on open.
pub fn body_blocks(xml: &str) -> Vec<Block> {
    let doc = format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{xml}</w:body></w:document>"
    );
    crate::load::parse_document_xml(&doc, &crate::load::Relationships::default()).body
}

/// Whether a block opens a cover page (ours or Word's).
pub fn is_cover_open(block: &Block) -> bool {
    matches!(block, Block::Raw(raw) if is_sdt_open(raw)
        && doc_part_gallery(raw).is_some_and(|g| g == COVER_GALLERY))
}

/// The document's cover page: the indexes of its opening and closing
/// boundaries among the top-level `blocks`. The first one, when there are
/// several; `None` when there is none, or it never closes.
pub fn find_cover(blocks: &[Block]) -> Option<(usize, usize)> {
    let open = blocks.iter().position(is_cover_open)?;
    Some((open, matching_close(blocks, open)?))
}

/// Whether `text` is something a person typed: not blank, and not a
/// bracketed prompt such as `[Document title]` (Word's, in every version,
/// are bracketed too: `[Type the document title]`, `[Pick the date]`).
fn is_typed(text: &str) -> bool {
    let t = text.trim();
    !(t.is_empty() || (t.starts_with('[') && t.ends_with(']')))
}

/// The placeholder a content control's opening boundary names: its
/// `w:alias`, else its `w:tag`.
fn placeholder_of(open: &str) -> Option<Placeholder> {
    let (a, b) = crate::sect::find_element(open, "w:sdtPr")?;
    let pr = &open[a..b];
    ["w:alias", "w:tag"].iter().find_map(|name| {
        let (c, d) = crate::sect::find_element(pr, name)?;
        Placeholder::from_alias(&crate::load::xml_attr_value(&pr[c..d], "w:val")?)
    })
}

/// Collect the text of every placeholder control in `content` into `out`,
/// innermost first, keeping the first text found for each placeholder.
fn inline_placeholders(content: &[Inline], out: &mut Typed) {
    let mut open: Vec<(Option<Placeholder>, String)> = Vec::new();
    for inline in content {
        match inline {
            Inline::Raw(raw) if is_sdt_open(raw) => open.push((placeholder_of(raw), String::new())),
            Inline::Raw(raw) if is_sdt_close(raw) => {
                if let Some((Some(field), text)) = open.pop() {
                    if is_typed(&text) {
                        out.entry(field).or_insert_with(|| text.trim().to_string());
                    }
                }
            }
            other => {
                let t = other.text();
                for (_, text) in open.iter_mut() {
                    text.push_str(&t);
                }
            }
        }
    }
}

/// The text typed into the placeholders of the cover `blocks` (from its
/// opening boundary through its closing one): inline placeholder controls,
/// and block-level ones such as Word's Abstract, whose paragraphs are joined
/// with line breaks. Blank and still-bracketed placeholders are left out.
/// `w:showingPlcHdr` is not consulted: the editor never clears it, so it
/// would hide text a person typed into a Word cover here.
pub fn typed_placeholders(blocks: &[Block]) -> Typed {
    let mut out = Typed::new();
    for (i, block) in blocks.iter().enumerate() {
        match block {
            Block::Raw(raw) if i > 0 && is_sdt_open(raw) => {
                let (Some(field), Some(end)) = (placeholder_of(raw), matching_close(blocks, i))
                else {
                    continue;
                };
                let text = blocks[i + 1..end]
                    .iter()
                    .filter(|b| matches!(b, Block::Paragraph(_) | Block::Table(_)))
                    .map(Block::plain_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                if is_typed(&text) {
                    out.entry(field).or_insert_with(|| text.trim().to_string());
                }
            }
            Block::Paragraph(p) => inline_placeholders(&p.content, &mut out),
            _ => {}
        }
    }
    out
}

/// Every content control `w:id` in `xml` (a part, or serialized blocks).
pub fn sdt_ids(xml: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<w:id ") {
        rest = &rest[at..];
        let end = rest.find('>').unwrap_or(rest.len());
        if let Some(v) = crate::load::xml_attr_value(&rest[..end], "w:val") {
            if let Ok(n) = v.trim().parse::<i64>() {
                out.push(n);
            }
        }
        rest = &rest[end..];
    }
    out
}

/// `n` content-control ids none of `used` holds: the ones after the largest,
/// else (that would pass Word's signed 32-bit range) the smallest free
/// positive ones.
pub fn free_sdt_ids(used: &[i64], n: usize) -> Vec<i64> {
    let max = used.iter().copied().max().unwrap_or(0).max(0);
    if max + (n as i64) < i64::from(i32::MAX) {
        return (1..=n as i64).map(|k| max + k).collect();
    }
    let taken: std::collections::HashSet<i64> = used.iter().copied().collect();
    (1..).filter(|k| !taken.contains(k)).take(n).collect()
}

#[cfg(test)]
pub(crate) mod tests;
