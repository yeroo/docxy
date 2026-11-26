//! Convert to editable document with the PDF's layout (#2257), in the
//! converting child (`convert_child`, `--convert-import pdf`).
//!
//! PdfCraft does most of it: `pdfcraft-edit` reads each page's paragraphs,
//! pictures, ruled lines and background, `pdfcraft-create` gives a
//! picture's PNG or JPEG bytes, and `pdfcraft-export` writes the Word
//! document: paragraphs in reading order with their size, weight, slant and
//! colour, headings, lists, tables from the layout and the rules, pictures
//! in place, and pages that cannot flow as positioned text boxes. What it
//! does not do is done here, on its pages before the export and on the
//! document after it:
//!
//! - running headers and footers (`running.rs`) leave the pages and become
//!   the document's header and footer parts, page numbers as fields;
//! - columns (`columns.rs`): a page is split into virtual pages the export
//!   reads as one column each, whose sections get the columns back;
//! - fonts (`fonts.rs`): the export's runs have none. Each block's colour is
//!   replaced by a code colour unique to it before the export, which only
//!   ever copies colours, so every run in the document says which block it
//!   came from; afterwards each coded run gets its block's real colour (none
//!   for black) and font family. A colour that is no code is left alone.
//!
//! When PdfCraft cannot read the file, fails, panics or keeps nothing (no
//! text, no picture), the text importer (`docxcore::import::import_pdf`,
//! #951) converts it as before.

use crate::convert_child::ConvertedDoc;
use docxcore::model::{Block, Inline, RunProps};
use docxcore::sect::{Columns, SectionSetup, SectionStart};
use std::collections::HashMap;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod columns;
mod fonts;
mod running;

/// How long the layout step may take: half the child's whole
/// [`crate::convert_child::TIMEOUT`], so a slow file leaves the text
/// importer the other half. Checked per page, before each page's pictures,
/// and before the export and its loading.
pub(crate) const LAYOUT_TIME: Duration =
    Duration::from_secs(crate::convert_child::TIMEOUT.as_secs() / 2);

/// What the layout step says past [`LAYOUT_TIME`].
pub(crate) const LAYOUT_TOO_LONG: &str = "the layout conversion took too long";

/// The most blocks that get a code colour: every 24-bit colour but black.
/// Blocks past it keep their colour and no font.
const MAX_CODES: usize = (1 << 24) - 1;

/// A block's source facts the export has no field for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Source {
    pub(crate) base_font: String,
    pub(crate) color: [f64; 3],
}

/// What a code colour stands for.
#[derive(Clone, Debug, PartialEq)]
struct Coded {
    font: Option<String>,
    /// `RRGGBB`, `None` for black.
    color: Option<String>,
    size: f64,
}

/// The PDF as read for the export: its pages, each block's source beside
/// it, and its title.
pub(crate) struct Read {
    pub(crate) pages: Vec<pdfcraft_export::Page>,
    pub(crate) sources: Vec<Vec<Source>>,
    pub(crate) title: String,
}

/// Convert `bytes` with the layout, or as the text importer does when that
/// fails.
pub(crate) fn convert(bytes: &[u8]) -> Result<ConvertedDoc, String> {
    convert_with(bytes, layout)
}

/// [`convert`] with the layout step `craft` (tests pass one that fails or
/// panics): its document, or else the text importer's result.
pub(crate) fn convert_with(
    bytes: &[u8],
    craft: impl FnOnce(&[u8]) -> Result<Option<ConvertedDoc>, String>,
) -> Result<ConvertedDoc, String> {
    match catch_unwind(AssertUnwindSafe(|| craft(bytes))) {
        Ok(Ok(Some(doc))) => Ok(doc),
        _ => docxcore::import::import_pdf(bytes).map(ConvertedDoc::plain),
    }
}

/// The PDF converted with its layout within [`LAYOUT_TIME`]; `Ok(None)`
/// when it keeps nothing.
pub(crate) fn layout(bytes: &[u8]) -> Result<Option<ConvertedDoc>, String> {
    layout_until(bytes, Instant::now() + LAYOUT_TIME)
}

/// Past `deadline`, an error ([`LAYOUT_TOO_LONG`]).
fn on_time(deadline: Instant) -> Result<(), String> {
    if Instant::now() < deadline {
        Ok(())
    } else {
        Err(LAYOUT_TOO_LONG.into())
    }
}

/// [`layout`], given up at `deadline`.
pub(crate) fn layout_until(
    bytes: &[u8],
    deadline: Instant,
) -> Result<Option<ConvertedDoc>, String> {
    let Read {
        mut pages,
        mut sources,
        title,
    } = read_pages(bytes, deadline)?;
    if pages
        .iter()
        .all(|p| p.blocks.is_empty() && p.images.is_empty())
    {
        return Ok(None);
    }
    // Running headers and footers leave the pages first, so neither the
    // columns nor the export's body size see them.
    let running = running::find(&pages);
    let mut looks = HashMap::new();
    if let Some(r) = &running {
        for piece in r.header.iter().chain(&r.footer).flatten() {
            let b = &pages[piece.page].blocks[piece.block];
            let s = &sources[piece.page][piece.block];
            looks.insert(
                (piece.page, piece.block),
                running::RunLook {
                    font: fonts::family(&s.base_font),
                    size: b.size,
                    bold: b.bold,
                    italic: b.italic,
                    color: hex(s.color),
                },
            );
        }
        for ((page, src), removed) in pages.iter_mut().zip(&mut sources).zip(&r.removed) {
            let mut keep = removed.iter().map(|r| !r);
            let mut keep_src = removed.iter().map(|r| !r);
            page.blocks.retain(|_| keep.next().unwrap_or(true));
            src.retain(|_| keep_src.next().unwrap_or(true));
        }
    }
    let first_width = pages.first().map_or(612.0, |p| p.width);
    let bodies: Vec<f64> = pages.iter().map(columns::body_size).collect();
    let mut virt = Vec::new();
    for (i, (page, src)) in pages.into_iter().zip(sources).enumerate() {
        virt.extend(columns::split(page, src, i));
    }
    // Code colours: block k (from 1) is colour k.
    let mut table: Vec<Coded> = Vec::new();
    for v in &mut virt {
        for (b, s) in v.page.blocks.iter_mut().zip(&v.sources) {
            if table.len() >= MAX_CODES {
                break;
            }
            table.push(Coded {
                font: fonts::family(&s.base_font),
                color: hex(s.color),
                size: b.size,
            });
            b.color = code_color(table.len());
        }
    }
    let export: Vec<pdfcraft_export::Page> = virt
        .iter_mut()
        .map(|v| std::mem::take(&mut v.page))
        .collect();
    on_time(deadline)?;
    let docx = pdfcraft_export::docx_with_language(&export, &title, None);
    if docx.is_empty() {
        return Err("PdfCraft could not write the converted document".into());
    }
    on_time(deadline)?;
    let mut pkg = docxcore::package::load_package(&docx).map_err(|e| e.to_string())?;
    pkg.ensure_styles(&["Heading1", "Heading2"]);
    sections(
        &mut pkg.document.body,
        &virt,
        &table,
        &bodies,
        running.as_ref(),
    );
    map_blocks(&mut pkg.document.body, &table);
    if let Some(r) = &running {
        add_running(&mut pkg, r, &looks, first_width);
    }
    let doc = std::mem::take(&mut pkg.document);
    Ok(Some(ConvertedDoc {
        doc,
        base: Some(pkg),
    }))
}

/// The PDF's pages as `pdfcraft_export` takes them, each block's source
/// beside it (same order), as `pdfcraft-engine`'s `export_pages_within`
/// builds them; pictures within `docxcore`'s HTML import budgets
/// (`html_image`), which the suite holds the converted document in too;
/// given up at `deadline`.
pub(crate) fn read_pages(bytes: &[u8], deadline: Instant) -> Result<Read, String> {
    use docxcore::import::html_image::{MAX_IMAGE_BYTES, MAX_MEDIA_BYTES};
    let bytes = Arc::new(bytes.to_vec());
    let cos = pdfcraft_cos::Document::open_with_password(bytes.clone(), None)
        .map_err(|e| e.to_string())?;
    let info = pdfcraft_render::inspect(bytes, None).map_err(|e| e.to_string())?;
    let mut encoded: HashMap<pdfcraft_cos::ObjRef, Option<Picture>> = HashMap::new();
    let mut work = pdfcraft_cos::ImageBudget::new(&cos);
    let mut left = MAX_MEDIA_BYTES;
    let mut pages = Vec::with_capacity(info.pages.len());
    let mut sources = Vec::with_capacity(info.pages.len());
    for (i, page) in info.pages.iter().enumerate() {
        on_time(deadline)?;
        let visible = |r: [f64; 4]| {
            let a = page.user_to_view(r[0] as f32, r[1] as f32);
            let b = page.user_to_view(r[2] as f32, r[3] as f32);
            let h = page.height as f64;
            [
                a[0].min(b[0]) as f64,
                h - a[1].max(b[1]) as f64,
                a[0].max(b[0]) as f64,
                h - a[1].min(b[1]) as f64,
            ]
        };
        let unit = display_unit(page);
        let reading = pdfcraft_edit::reading_blocks_checked(&cos, i).unwrap_or_default();
        let mut blocks = Vec::new();
        let mut src = Vec::new();
        for b in reading.blocks {
            if b.text.trim().is_empty() {
                continue;
            }
            let (bold, italic) = weight_and_slant(&b.base_font);
            let space_around = [
                b.editor_text.starts_with(char::is_whitespace),
                b.editor_text.ends_with(char::is_whitespace),
            ];
            src.push(Source {
                base_font: b.base_font,
                color: b.color,
            });
            blocks.push(pdfcraft_export::Block {
                space_around,
                text: b.text,
                rect: visible(b.rect),
                size: b.size * unit,
                bold,
                italic,
                color: b.color,
                rotation: b.rotation - f64::from(page.rotation),
            });
        }
        on_time(deadline)?;
        let placed = pdfcraft_edit::reading_images(&cos, i).unwrap_or_default();
        let mut images = Vec::new();
        for im in &placed {
            on_time(deadline)?;
            let Some(object) = im.object else { continue };
            let prepared = encoded.entry(object).or_insert_with(|| {
                let (ext, data) =
                    pdfcraft_create::image_file_within(&cos, object, &mut work).ok()?;
                if data.len() > MAX_IMAGE_BYTES {
                    return None;
                }
                left = left.checked_sub(data.len())?;
                Some((if ext == "jpg" { "jpg" } else { "png" }, Arc::from(data)))
            });
            let Some((ext, data)) = prepared.clone() else {
                continue;
            };
            let m = im.matrix;
            images.push(pdfcraft_export::Image {
                ext,
                bytes: data,
                rect: visible(im.rect),
                size: [m[0].hypot(m[1]) * unit, m[2].hypot(m[3]) * unit],
                rotation: m[1].atan2(m[0]).to_degrees() - f64::from(page.rotation),
            });
        }
        let background = pdfcraft_edit::page_background(&cos, i).ok().flatten();
        let rules = pdfcraft_edit::page_rules(&cos, i)
            .unwrap_or_default()
            .into_iter()
            .map(visible)
            .collect();
        pages.push(pdfcraft_export::Page {
            width: page.width as f64,
            height: page.height as f64,
            blocks,
            images,
            background,
            rules,
        });
        sources.push(src);
    }
    Ok(Read {
        pages,
        sources,
        title: info.title.unwrap_or_default(),
    })
}

/// A picture's extension and bytes, encoded once per image object.
type Picture = (&'static str, Arc<[u8]>);

/// Displayed points per user-space unit on `page` (`/UserUnit`), 1 when the
/// page has no usable size: `pdfcraft-engine`'s `display_unit`, which is
/// not public.
fn display_unit(page: &pdfcraft_render::PageInfo) -> f64 {
    let crop = f64::from(page.crop[2] - page.crop[0]);
    let shown = f64::from(if page.rotation.is_multiple_of(180) {
        page.width
    } else {
        page.height
    });
    if crop > 0.0 && crop.is_finite() && shown.is_finite() {
        shown / crop
    } else {
        1.0
    }
}

/// Bold and italic as a font's name says them (`Arial-BoldItalicMT`):
/// `pdfcraft-engine`'s `weight_and_slant`, which is not public.
pub(crate) fn weight_and_slant(base_font: &str) -> (bool, bool) {
    let f = base_font.to_ascii_lowercase();
    (
        f.contains("bold") || f.contains("black") || f.contains("heavy"),
        f.contains("italic") || f.contains("oblique"),
    )
}

/// A colour's 8-bit components as the export rounds them.
fn rgb8(c: [f64; 3]) -> [u8; 3] {
    c.map(|v| {
        if v.is_finite() {
            (v.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            0
        }
    })
}

/// The colour as `RRGGBB`, `None` for black.
fn hex(c: [f64; 3]) -> Option<String> {
    let [r, g, b] = rgb8(c);
    (r | g | b != 0).then(|| format!("{r:02X}{g:02X}{b:02X}"))
}

/// Code colour `k` (from 1): its 24 bits as the colour's components.
fn code_color(k: usize) -> [f64; 3] {
    [16, 8, 0].map(|s| ((k >> s) & 0xFF) as f64 / 255.0)
}

/// The code colour `RRGGBB` stands for, if it is one of `n`.
fn code_of(color: &str, n: usize) -> Option<usize> {
    let k = usize::from_str_radix(color, 16).ok()?;
    (color.len() == 6 && (1..=n).contains(&k)).then_some(k)
}

/// The coded block a run came from.
fn coded<'a>(props: &RunProps, table: &'a [Coded]) -> Option<&'a Coded> {
    let k = code_of(props.color.as_deref()?, table.len())?;
    table.get(k - 1)
}

/// A coded run's real colour and its font.
fn map_props(props: &mut RunProps, table: &[Coded]) {
    if let Some(c) = coded(props, table) {
        props.color = c.color.clone();
        if c.font.is_some() {
            props.font = c.font.clone();
        }
    }
}

fn map_blocks(blocks: &mut [Block], table: &[Coded]) {
    for b in blocks {
        match b {
            Block::Paragraph(p) => map_inlines(&mut p.content, table),
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        map_blocks(&mut cell.blocks, table);
                    }
                }
            }
            Block::SectionProperties(_) | Block::Raw(_) => {}
        }
    }
}

fn map_inlines(items: &mut [Inline], table: &[Coded]) {
    for item in items {
        match item {
            Inline::Run(r) => map_props(&mut r.props, table),
            Inline::Hyperlink(h) => {
                for r in &mut h.runs {
                    map_props(&mut r.props, table);
                }
                map_inlines(&mut h.content, table);
            }
            Inline::Break(_, props) | Inline::Tab(props) => map_props(props, table),
            Inline::TextBox { raw, blocks } => {
                let twins = twin_copies(raw);
                map_blocks(blocks, table);
                // docxcore writes the blocks back into the first copy only;
                // the others (a VML `mc:Fallback`) are kept as loaded, so
                // they are written here as the first copy will be.
                if !twins.is_empty() {
                    let inner = docxcore::serialize::blocks_to_xml(blocks);
                    for r in twins.into_iter().rev() {
                        raw.replace_range(r, &inner);
                    }
                }
            }
            Inline::Revision { content, .. } => map_inlines(content, table),
            _ => {}
        }
    }
}

/// The `w:txbxContent` copies in a text box's XML that are byte for byte
/// the first one, the first included (the inner ranges); none when one has
/// another nested in it.
fn twin_copies(raw: &str) -> Vec<Range<usize>> {
    const OPEN: &str = "<w:txbxContent>";
    const CLOSE: &str = "</w:txbxContent>";
    let mut inners = Vec::new();
    let mut from = 0;
    while let Some(a) = raw[from..].find(OPEN).map(|a| from + a + OPEN.len()) {
        let Some(b) = raw[a..].find(CLOSE).map(|b| a + b) else {
            break;
        };
        if raw[a..b].contains("<w:txbxContent") {
            return Vec::new();
        }
        inners.push(a..b);
        from = b + CLOSE.len();
    }
    let Some(first) = inners.first().map(|r| &raw[r.clone()]) else {
        return Vec::new();
    };
    inners
        .iter()
        .filter(|r| &raw[(*r).clone()] == first)
        .cloned()
        .collect()
}

/// Points as twips.
fn twips(pt: f64) -> i32 {
    if pt.is_finite() {
        (pt * 20.0).round().clamp(0.0, 31680.0) as i32
    } else {
        0
    }
}

/// The sections the export made, one per virtual page: a column region's
/// columns, start and real page; the header and footer distances; and the
/// heading levels of a band's paragraphs, which the export, sizing headings
/// against the band alone, would lose. A document whose sections do not
/// match the virtual pages one to one is left as it is.
fn sections(
    body: &mut [Block],
    virt: &[columns::Virtual],
    table: &[Coded],
    bodies: &[f64],
    running: Option<&running::Running>,
) {
    let ends: Vec<usize> = body
        .iter()
        .enumerate()
        .filter(|(_, b)| match b {
            Block::Paragraph(p) => p.props.section_break.is_some(),
            Block::SectionProperties(_) => true,
            _ => false,
        })
        .map(|(i, _)| i)
        .collect();
    if ends.len() != virt.len() {
        return;
    }
    let mut start = 0;
    for (v, &end) in virt.iter().zip(&ends) {
        let band = v.cols.is_none() && virt.iter().any(|w| w.from == v.from && w.continuous);
        if band {
            let page_body = bodies.get(v.from).copied().unwrap_or(11.0).max(1.0);
            for b in &mut body[start..end] {
                let Block::Paragraph(p) = b else { continue };
                let plain = p.props.style_id.is_none()
                    || p.props
                        .style_id
                        .as_deref()
                        .is_some_and(|s| s.starts_with("Heading"));
                let first = p.content.iter().find_map(|i| match i {
                    Inline::Run(r) => coded(&r.props, table).map(|c| (c.size, r.props.bold)),
                    _ => None,
                });
                let Some((size, bold)) = first else { continue };
                let level = heading_level(size, bold, &p.plain_text(), page_body);
                if plain && level > 0 {
                    p.props.heading_level = Some(level);
                    p.props.style_id = Some(format!("Heading{level}"));
                }
            }
        }
        let raw = match &mut body[end] {
            Block::Paragraph(p) => p.props.section_break.as_mut(),
            Block::SectionProperties(s) => Some(&mut s.raw),
            _ => None,
        };
        if let Some(raw) = raw {
            let mut setup = SectionSetup::parse(raw);
            if let Some((n, space)) = v.cols {
                setup.columns = Columns::default().equal(n as i32, twips(space));
                if v.continuous {
                    setup.start = SectionStart::Continuous;
                }
                // The real page and PdfCraft's margins for it.
                let (w, h) = (twips(v.real[0]).max(144), twips(v.real[1]).max(144));
                setup.page.w = w;
                setup.page.h = h;
                let (mx, my) = ((w / 8).min(1440), (h / 8).min(1440));
                setup.margins.left = mx;
                setup.margins.right = mx;
                setup.margins.top = my;
                setup.margins.bottom = my;
                setup.margins.header = my / 2;
                setup.margins.footer = my / 2;
            }
            if let Some(r) = running {
                if let Some(d) = r.header_from_top {
                    setup.margins.header = twips(d);
                }
                if let Some(d) = r.footer_from_bottom {
                    setup.margins.footer = twips(d);
                }
            }
            *raw = setup.apply(raw);
        }
        start = end + 1;
    }
}

/// A paragraph's heading level against its page's body size, as
/// `pdfcraft_export`'s `level` decides it (not public): 1 at 1.6 times the
/// body, 2 at 1.25 times or a short bold line at least the body's size that
/// ends no sentence; only for text under 200 bytes.
fn heading_level(size: f64, bold: bool, text: &str, body: f64) -> u8 {
    let text = text.trim();
    let short = text.len() < 200;
    if short && size >= body * 1.6 {
        1
    } else if short
        && (size >= body * 1.25
            || (bold && size >= body && text.len() < 80 && !text.ends_with('.')))
    {
        2
    } else {
        0
    }
}

/// The running header and footer as parts referenced from the first
/// section (the others inherit them), with an empty first-page header and
/// footer for a title page.
fn add_running(
    pkg: &mut docxcore::package::Package,
    r: &running::Running,
    looks: &HashMap<(usize, usize), running::RunLook>,
    first_width: f64,
) {
    let w = twips(first_width).max(144);
    let text_width = w - 2 * (w / 8).min(1440);
    let look = |p: &running::Piece| match looks.get(&(p.page, p.block)) {
        Some(l) => running::RunLook {
            font: l.font.clone(),
            size: l.size,
            bold: l.bold,
            italic: l.italic,
            color: l.color.clone(),
        },
        None => running::RunLook {
            font: None,
            size: 11.0,
            bold: false,
            italic: false,
            color: None,
        },
    };
    pkg.ensure_styles(&[crate::hf::style_id(true), crate::hf::style_id(false)]);
    let mut refs = Vec::new();
    for (lines, is_header) in [(&r.header, true), (&r.footer, false)] {
        if lines.is_empty() {
            continue;
        }
        let xml = running::part_xml(lines, is_header, text_width, &look);
        if let Some((rid, _)) = pkg.create_hf_part(is_header, &xml) {
            refs.push((is_header, "default", rid));
        }
        if r.title_page {
            if let Some((rid, _)) =
                pkg.create_hf_part(is_header, &crate::hf::empty_content(is_header))
            {
                refs.push((is_header, "first", rid));
            }
        }
    }
    let first = pkg.document.body.iter_mut().find_map(|b| match b {
        Block::Paragraph(p) => p.props.section_break.as_mut(),
        Block::SectionProperties(s) => Some(&mut s.raw),
        _ => None,
    });
    let Some(first) = first else { return };
    for (is_header, variant, rid) in refs {
        *first = docxcore::sect::set_hf_reference(first, is_header, variant, Some(&rid));
    }
    if r.title_page {
        *first = docxcore::sect::set_flag(first, "w:titlePg", true);
    }
}

#[cfg(test)]
mod tests;
