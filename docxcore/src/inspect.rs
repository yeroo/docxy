//! Word's Document Inspector (File > Info > Inspect Document): find and
//! remove comments, hidden text and personal document properties. Tracked
//! changes are resolved by accepting them ([`Document::accept_all_revisions`]),
//! so they need nothing here beyond a count.
//!
//! The body walk descends into the modeled tree: paragraphs, table cells,
//! hyperlinks (both `runs` and `content`), tracked change wrappers and text
//! boxes. A hyperlink or revision wrapper that loaded from the file saves its
//! original `raw` XML until `content_changed` is set, so removing anything
//! inside one sets that flag; otherwise the save would quietly write the
//! removed content back.
//!
//! Some content stays raw XML: fields, tracked moves, unmodeled inline and
//! block elements, and a group shape's other text boxes. Comment markers are
//! cut out of that XML too. Hidden runs in it are counted
//! ([`count_unremovable_hidden_runs`]) but not removed, since that would
//! mean editing run XML the model doesn't hold.

use crate::model::{Block, Document, Inline, Run, RunProps};
use crate::package::Package;
use crate::xml::{Event, XmlParser};
use std::ops::Range;

/// What a body walk removes or counts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target<'a> {
    /// Runs, tabs and breaks formatted `w:vanish`.
    Hidden,
    /// `w:commentRangeStart`, `w:commentRangeEnd` and `w:commentReference`:
    /// every comment's, or only those whose `w:id` is the given one.
    CommentMarkers(Option<&'a str>),
    /// Hidden runs inside raw XML ([`count_hidden_in_xml`]). Counted only.
    RawHidden,
}

impl Target<'_> {
    /// Whether the walk drops this modeled inline whole. Comment markers
    /// live in raw XML and are stripped from it instead ([`strip_markers`]).
    fn matches(self, inline: &Inline) -> bool {
        match (self, inline) {
            (Target::Hidden, Inline::Run(r)) => is_hidden(&r.props),
            (Target::Hidden, Inline::Tab(p) | Inline::Break(_, p)) => is_hidden(p),
            _ => false,
        }
    }

    fn matches_run(self, run: &Run) -> bool {
        self == Target::Hidden && is_hidden(&run.props)
    }
}

/// Whether direct run formatting hides the text the way Word's inspector
/// means it: `w:vanish`. `w:webHidden` (hidden only in Web layout; every TOC
/// puts it on its page numbers) is not hidden text and is not `vanish`.
fn is_hidden(props: &RunProps) -> bool {
    props.vanish
}

const MARKER_TAGS: [&str; 3] = [
    "w:commentRangeStart",
    "w:commentRangeEnd",
    "w:commentReference",
];

/// Comment marker elements in raw XML: every comment's, or only `id`'s.
fn count_markers(xml: &str, id: Option<&str>) -> usize {
    MARKER_TAGS
        .iter()
        .map(|name| {
            let (mut n, mut from) = (0, 0);
            while let Some((start, end, _)) = find_element_from(xml, name, from) {
                n += usize::from(has_id(&xml[start..end], id));
                from = end;
            }
            n
        })
        .sum()
}

/// `xml` without its comment marker elements (every comment's, or only
/// `id`'s), and how many there were. Only the markers go: a raw run that
/// holds text and a `w:commentReference`, or a `w:customXml` wrapper around
/// a commented range, keeps everything else.
fn strip_markers(xml: &str, id: Option<&str>) -> (String, usize) {
    match id {
        None => remove_elements(xml, &MARKER_TAGS),
        Some(id) => remove_markers_of(xml, &MARKER_TAGS, id),
    }
}

/// Whether the element at the start of `element` has `w:id` equal to `id`
/// (always, for no `id`). Only its start tag is read, so attribute order
/// and quote style don't matter, and `1` is not `10`.
fn has_id(element: &str, id: Option<&str>) -> bool {
    let Some(id) = id else {
        return true;
    };
    let Some(head) = tag_end(element) else {
        return false;
    };
    let mut p = XmlParser::new(&element[..head]);
    p.next() == Event::Start && p.attr("w:id") == id
}

/// `xml` without any element named in `names` whose `w:id` is `id`, and how
/// many there were. Elements of other ids stay.
fn remove_markers_of(xml: &str, names: &[&str], id: &str) -> (String, usize) {
    let mut out = xml.to_string();
    let mut removed = 0;
    for name in names {
        let mut from = 0;
        while let Some((start, end, _)) = find_element_from(&out, name, from) {
            if has_id(&out[start..end], Some(id)) {
                out.replace_range(start..end, "");
                removed += 1;
                from = start;
            } else {
                from = end;
            }
        }
    }
    (out, removed)
}

/// `xml` without any element named in `names`, and how many there were.
fn remove_elements(xml: &str, names: &[&str]) -> (String, usize) {
    let mut out = xml.to_string();
    let mut removed = 0;
    for name in names {
        while let Some((start, end, _)) = find_element_from(&out, name, 0) {
            out.replace_range(start..end, "");
            removed += 1;
        }
    }
    (out, removed)
}

/// Whether what stripping left of a raw inline is nothing worth keeping:
/// nothing at all, or a `w:r` with no child but its `w:rPr` (the run a
/// `w:commentReference` sat in).
fn is_leftover(xml: &str) -> bool {
    let xml = xml.trim();
    if xml.is_empty() {
        return true;
    }
    let mut p = XmlParser::new(xml);
    if p.next() != Event::Start || p.name() != "w:r" {
        return false;
    }
    loop {
        match p.next() {
            Event::Start if p.name() == "w:rPr" => p.skip_element(),
            Event::Start | Event::Eof => return false,
            Event::End => return xml[p.pos()..].trim().is_empty(),
            Event::Text => {}
        }
    }
}

/// Hidden runs in raw XML, by the rule of [`is_hidden`]: a `w:r` whose own
/// `w:rPr` turns `w:vanish` on, whatever its `w:webHidden`. Only leaf runs
/// count: a drawing run that holds a text box's runs is not itself text.
fn count_hidden_in_xml(xml: &str) -> usize {
    struct RunFrame {
        depth: usize,
        vanish: bool,
        has_inner_run: bool,
    }
    let mut p = XmlParser::new(xml);
    let mut names: Vec<&str> = Vec::new();
    let mut runs: Vec<RunFrame> = Vec::new();
    let mut count = 0;
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name();
                // The run's own properties: `w:r` > `w:rPr` > this element.
                let own_prop = names.len() >= 2
                    && names[names.len() - 1] == "w:rPr"
                    && names[names.len() - 2] == "w:r";
                if let Some(run) = runs.last_mut().filter(|_| own_prop && name == "w:vanish") {
                    run.vanish = !matches!(p.attr("w:val"), "0" | "false" | "off");
                }
                names.push(name);
                if name == "w:r" {
                    if let Some(outer) = runs.last_mut() {
                        outer.has_inner_run = true;
                    }
                    runs.push(RunFrame {
                        depth: names.len(),
                        vanish: false,
                        has_inner_run: false,
                    });
                }
            }
            Event::End => {
                if let Some(run) = runs.pop_if(|r| r.depth == names.len())
                    && run.vanish
                    && !run.has_inner_run
                {
                    count += 1;
                }
                names.pop();
            }
            Event::Text => {}
            Event::Eof => return count,
        }
    }
}

/// What `target` counts in a piece of raw XML.
fn count_in_raw(xml: &str, target: Target) -> usize {
    match target {
        Target::CommentMarkers(id) => count_markers(xml, id),
        Target::RawHidden => count_hidden_in_xml(xml),
        Target::Hidden => 0,
    }
}

fn count_blocks(blocks: &[Block], target: Target) -> usize {
    blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => count_inlines(&p.content, target),
            Block::Table(t) => t
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .map(|cell| count_blocks(&cell.blocks, target))
                .sum(),
            Block::Raw(raw) => count_in_raw(raw, target),
            Block::SectionProperties(_) => 0,
        })
        .sum()
}

/// The raw XML of an inline that the walk strips comment markers from.
/// Hyperlinks and revisions are not here: their children are walked, and a
/// changed wrapper is rebuilt from them. Text boxes are not either: their
/// raw holds copies of their blocks ([`text_box_extra`]).
fn marker_raw(inline: &Inline) -> Option<&str> {
    match inline {
        Inline::Raw(raw) | Inline::Field { raw, .. } | Inline::UnsupportedRevision { raw, .. } => {
            Some(raw)
        }
        _ => None,
    }
}

fn marker_raw_mut(inline: &mut Inline) -> Option<&mut String> {
    match inline {
        Inline::Raw(raw) | Inline::Field { raw, .. } | Inline::UnsupportedRevision { raw, .. } => {
            Some(raw)
        }
        _ => None,
    }
}

fn count_inlines(content: &[Inline], target: Target) -> usize {
    content
        .iter()
        .map(|inline| {
            let own = match marker_raw(inline) {
                Some(raw) => count_in_raw(raw, target),
                None => usize::from(target.matches(inline)),
            };
            own + match inline {
                Inline::Hyperlink(h) => {
                    h.runs.iter().filter(|r| target.matches_run(r)).count()
                        + count_inlines(&h.content, target)
                }
                Inline::Revision { content, .. } => count_inlines(content, target),
                Inline::TextBox { raw, blocks } => {
                    count_blocks(blocks, target) + text_box_extra(raw, target)
                }
                _ => 0,
            }
        })
        .sum()
}

fn remove_blocks(blocks: &mut [Block], target: Target) -> usize {
    let mut removed = 0;
    for b in blocks {
        match b {
            Block::Paragraph(p) => removed += remove_inlines(&mut p.content, target),
            Block::Table(t) => {
                for cell in t.rows.iter_mut().flat_map(|row| &mut row.cells) {
                    removed += remove_blocks(&mut cell.blocks, target);
                }
            }
            // A body-level range marker (one before a table) is a raw block.
            // An emptied block stays, as an empty one, so the block paths
            // the editor holds keep pointing where they did.
            Block::Raw(raw) => {
                let Target::CommentMarkers(id) = target else {
                    continue;
                };
                let (out, n) = strip_markers(raw, id);
                if n > 0 {
                    *raw = if out.trim().is_empty() {
                        String::new()
                    } else {
                        out
                    };
                    removed += n;
                }
            }
            Block::SectionProperties(_) => {}
        }
    }
    removed
}

fn remove_inlines(content: &mut Vec<Inline>, target: Target) -> usize {
    let mut removed = 0;
    content.retain_mut(|inline| {
        if target.matches(inline) {
            removed += 1;
            return false;
        }
        let Target::CommentMarkers(id) = target else {
            return true;
        };
        let plain_raw = matches!(inline, Inline::Raw(_));
        let Some(raw) = marker_raw_mut(inline) else {
            return true;
        };
        let (out, n) = strip_markers(raw, id);
        if n == 0 {
            return true;
        }
        removed += n;
        if plain_raw && is_leftover(&out) {
            return false;
        }
        *raw = out;
        true
    });
    for inline in content.iter_mut() {
        match inline {
            Inline::Hyperlink(h) => {
                let runs_before = h.runs.len();
                h.runs.retain(|r| !target.matches_run(r));
                let inside = runs_before - h.runs.len() + remove_inlines(&mut h.content, target);
                if inside > 0 {
                    h.content_changed = true;
                }
                removed += inside;
            }
            Inline::Revision {
                content,
                content_changed,
                ..
            } => {
                let inside = remove_inlines(content, target);
                if inside > 0 {
                    *content_changed = true;
                }
                removed += inside;
            }
            Inline::TextBox { raw, blocks } => {
                // Judged on the loaded XML, before the strip below can make
                // twin copies differ.
                let twins: Vec<bool> = text_box_copies(raw).iter().map(|c| c.twin).collect();
                let mut inside = remove_blocks(blocks, target);
                if let Target::CommentMarkers(id) = target {
                    inside += text_box_extra(raw, target);
                    *raw = strip_markers(raw, id).0;
                }
                if inside > 0 {
                    sync_twin_copies(raw, blocks, &twins);
                }
                removed += inside;
            }
            _ => {}
        }
    }
    removed
}

/// One `w:txbxContent` element of a text box's `raw`.
struct TextBoxCopy {
    /// Byte range of its inner XML.
    inner: Range<usize>,
    /// Whether its inner XML equals the first copy's apart from `w14:paraId`
    /// attributes (see `without_para_ids`), so it shows the same `blocks`.
    /// The first copy is its own twin. Only twins may be rewritten
    /// from `blocks`; another box of a group keeps its own content.
    twin: bool,
}

/// The `w:txbxContent` copies in a text box's `raw`. The loader reads
/// `blocks` from the first one and save splices them back there only. Word
/// 2010+ writes a shape twice, a DrawingML `mc:Choice` and a VML
/// `mc:Fallback`; a group shape holds several boxes, each written twice.
/// Self-closing copies are skipped. Empty when the first copy has another
/// nested in it, a shape the save's splice can't handle either.
fn text_box_copies(raw: &str) -> Vec<TextBoxCopy> {
    const NAME: &str = "w:txbxContent";
    let mut inners: Vec<Range<usize>> = Vec::new();
    let mut from = 0;
    while let Some((start, end, _)) = find_element_from(raw, NAME, from) {
        from = end;
        let Some(head) = tag_end(&raw[start..]) else {
            break;
        };
        if raw[..start + head].ends_with("/>") {
            continue;
        }
        let inner = start + head..end - NAME.len() - "</>".len();
        if inners.is_empty() && raw[inner.clone()].contains(&format!("<{NAME}")) {
            return Vec::new();
        }
        inners.push(inner);
    }
    let Some(first) = inners.first().map(|r| &raw[r.clone()]) else {
        return Vec::new();
    };
    // A save may drop a repeated `w14:paraId` from one copy but not the other
    // (an id is never dropped in an `mc:Fallback`, #1063); that alone does not
    // make the copies differ.
    let first = without_para_ids(first);
    inners
        .iter()
        .map(|inner| TextBoxCopy {
            twin: without_para_ids(&raw[inner.clone()]) == first,
            inner: inner.clone(),
        })
        .collect()
}

/// `xml` without the `w14:paraId` attributes of its start tags (text is
/// left alone). Malformed XML is returned as it is.
fn without_para_ids(xml: &str) -> String {
    use crate::xml::{Event, XmlParser};
    let mut cuts = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => {
                for a in p.attrs().iter().filter(|a| a.name == "w14:paraId") {
                    let Some(cut) = crate::serialize::attr_source_range(xml, a) else {
                        return xml.to_string();
                    };
                    cuts.push(cut);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if p.is_malformed() {
        return xml.to_string();
    }
    crate::serialize::without_ranges(xml, &cuts)
}

/// What `target` counts in a text box's `raw` beyond what its `blocks`
/// stand for: everything but the first copy and its twins (another box of a
/// group, the shape around them).
fn text_box_extra(raw: &str, target: Target) -> usize {
    let copies = text_box_copies(raw);
    let Some(first) = copies.first() else {
        return 0;
    };
    let twins = copies.iter().filter(|c| c.twin).count();
    count_in_raw(raw, target)
        .saturating_sub(twins * count_in_raw(&raw[first.inner.clone()], target))
}

/// Rewrite every twin of the first copy (`twins`, read from the loaded XML)
/// from `blocks`, the first included, so the copies stay byte-identical and
/// a later removal still finds them twins, and a removal reaches the copy an
/// older reader shows. Any other copy stays as it is.
fn sync_twin_copies(raw: &mut String, blocks: &[Block], twins: &[bool]) {
    let copies = text_box_copies(raw);
    if copies.len() != twins.len() {
        return;
    }
    let inner = crate::serialize::blocks_to_xml(blocks);
    // Last first, so the earlier ranges stay valid.
    for (copy, _) in copies.iter().zip(twins).filter(|(_, t)| **t).rev() {
        raw.replace_range(copy.inner.clone(), &inner);
    }
}

/// Hidden runs, tabs and breaks in the modeled body (see [`is_hidden`]):
/// the ones [`remove_hidden_text`] removes.
pub fn count_hidden_runs(doc: &Document) -> usize {
    count_blocks(&doc.body, Target::Hidden)
}

/// Hidden runs that stay raw XML (fields, tracked moves, unmodeled
/// elements, a group shape's other text boxes): found, but not removed by
/// [`remove_hidden_text`].
pub fn count_unremovable_hidden_runs(doc: &Document) -> usize {
    count_blocks(&doc.body, Target::RawHidden)
}

/// Remove every hidden run, tab and break from the modeled body (see
/// [`count_hidden_runs`]). Returns how many.
pub fn remove_hidden_text(doc: &mut Document) -> usize {
    remove_blocks(&mut doc.body, Target::Hidden)
}

/// Whether any comment marker (range start/end or reference) is in the body.
pub fn has_comment_markers(doc: &Document) -> bool {
    count_blocks(&doc.body, Target::CommentMarkers(None)) > 0
}

/// The `w:id` of every comment marker (range start/end or reference) in the
/// body, wherever [`has_comment_markers`] would find it. A host uses it to
/// tell whether a comment it added is still anchored (#620).
pub fn comment_marker_ids(doc: &Document) -> std::collections::BTreeSet<String> {
    comment_marker_ids_in_blocks(&doc.body)
}

/// [`comment_marker_ids`] of any block list: a header's or footer's
/// content, say.
pub fn comment_marker_ids_in_blocks(blocks: &[Block]) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    marker_ids_in_blocks(blocks, &mut ids);
    ids
}

/// The `w:id` of every comment marker in a part's raw XML (a header or
/// footer part as the package holds it).
pub fn comment_marker_ids_in_xml(xml: &str) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    marker_ids_in_xml(xml, &mut ids);
    ids
}

fn marker_ids_in_blocks(blocks: &[Block], ids: &mut std::collections::BTreeSet<String>) {
    for b in blocks {
        match b {
            Block::Paragraph(p) => marker_ids_in_inlines(&p.content, ids),
            Block::Table(t) => {
                for cell in t.rows.iter().flat_map(|row| &row.cells) {
                    marker_ids_in_blocks(&cell.blocks, ids);
                }
            }
            Block::Raw(raw) => marker_ids_in_xml(raw, ids),
            Block::SectionProperties(_) => {}
        }
    }
}

fn marker_ids_in_inlines(content: &[Inline], ids: &mut std::collections::BTreeSet<String>) {
    for inline in content {
        if let Some(raw) = marker_raw(inline) {
            marker_ids_in_xml(raw, ids);
        }
        match inline {
            Inline::Hyperlink(h) => marker_ids_in_inlines(&h.content, ids),
            Inline::Revision { content, .. } => marker_ids_in_inlines(content, ids),
            // The raw holds the loaded copies; the blocks hold any edit.
            Inline::TextBox { raw, blocks } => {
                marker_ids_in_xml(raw, ids);
                marker_ids_in_blocks(blocks, ids);
            }
            _ => {}
        }
    }
}

fn marker_ids_in_xml(xml: &str, ids: &mut std::collections::BTreeSet<String>) {
    for name in MARKER_TAGS {
        let mut from = 0;
        while let Some((start, end, _)) = find_element_from(xml, name, from) {
            if let Some(head) = tag_end(&xml[start..end]) {
                let mut p = XmlParser::new(&xml[start..start + head]);
                if p.next() == Event::Start {
                    ids.insert(p.attr("w:id").to_string());
                }
            }
            from = end;
        }
    }
}

/// Remove every comment marker of every comment from the body. Returns how many.
pub fn remove_all_comment_markers(doc: &mut Document) -> usize {
    remove_blocks(&mut doc.body, Target::CommentMarkers(None))
}

/// Remove the markers of comment `id` from the body, wherever they are, and
/// nothing else: text that shares raw XML with a marker stays. Returns how
/// many.
pub fn remove_comment_markers(doc: &mut Document, id: &str) -> usize {
    remove_blocks(&mut doc.body, Target::CommentMarkers(Some(id)))
}

/// The byte length of the tag starting at `s[0] == '<'`, through its `>`,
/// skipping `>` inside quoted attribute values.
pub(crate) fn tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return Some(i + 1),
            _ => {}
        }
    }
    None
}

const CORE_PART: &str = "docProps/core.xml";
const APP_PART: &str = "docProps/app.xml";
const CUSTOM_PART: &str = "docProps/custom.xml";

/// The personal properties Word's inspector removes from `core.xml`. The
/// creation/modification dates and `cp:revision` stay. The prefixes are
/// matched literally: Word and the other producers we know write exactly
/// these.
const CORE_PERSONAL: [&str; 8] = [
    "dc:creator",
    "cp:lastModifiedBy",
    "dc:title",
    "dc:subject",
    "cp:keywords",
    "dc:description",
    "cp:category",
    "cp:contentStatus",
];
const APP_PERSONAL: [&str; 2] = ["Company", "Manager"];
const CUSTOM_PERSONAL: [&str; 1] = ["property"];

/// The first element named `name` at or after byte `from`: its byte range
/// and whether it has non-blank content. Unlike [`crate::sect::find_element`]
/// it starts at `from`, skips a `>` inside a quoted attribute value, and
/// finds nothing when the close tag is missing.
pub(crate) fn find_element_from(
    xml: &str,
    name: &str,
    from: usize,
) -> Option<(usize, usize, bool)> {
    let open = format!("<{name}");
    let mut at = from;
    loop {
        let start = at + xml[at..].find(&open)?;
        let after = start + open.len();
        // `<dc:title` must not match `<dc:titleX`.
        if !xml[after..].starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/') {
            at = after;
            continue;
        }
        let head_end = start + tag_end(&xml[start..])?;
        if xml[..head_end].ends_with("/>") {
            return Some((start, head_end, false));
        }
        let close = format!("</{name}>");
        let close_at = head_end + xml[head_end..].find(&close)?;
        let filled = !xml[head_end..close_at].trim().is_empty();
        return Some((start, close_at + close.len(), filled));
    }
}

fn has_filled(xml: &str, names: &[&str]) -> bool {
    names.iter().any(|name| {
        let mut from = 0;
        while let Some((_, end, filled)) = find_element_from(xml, name, from) {
            if filled {
                return true;
            }
            from = end;
        }
        false
    })
}

fn property_parts() -> [(&'static str, &'static [&'static str]); 3] {
    [
        (CORE_PART, &CORE_PERSONAL),
        (APP_PART, &APP_PERSONAL),
        (CUSTOM_PART, &CUSTOM_PERSONAL),
    ]
}

/// Whether the package holds personal document properties: a non-empty
/// author, title, … in `core.xml`, a company or manager in `app.xml`, or any
/// custom property. Empty elements (`<dc:title/>`) don't count.
pub fn has_personal_properties(pkg: &Package) -> bool {
    property_parts().iter().any(|(part, names)| {
        pkg.part_text(part)
            .is_some_and(|xml| has_filled(&xml, names))
    })
}

/// Remove the personal document properties (see [`has_personal_properties`]),
/// empty ones included. Returns whether any part changed.
pub fn remove_personal_properties(pkg: &mut Package) -> bool {
    let mut changed = false;
    for (part, names) in property_parts() {
        let Some(xml) = pkg.part_text(part) else {
            continue;
        };
        let (out, _) = remove_elements(&xml, names);
        if out != xml {
            changed |= pkg.set_part_text(part, &out);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::{Relationships, parse_document_xml};
    use crate::package::load_package;
    use crate::serialize::document_to_xml;
    use crate::zipwrite::write_zip;

    const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
                     xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";

    fn parse(body: &str) -> Document {
        let xml = format!("<w:document {W}><w:body>{body}</w:body></w:document>");
        parse_document_xml(&xml, &Relationships::default())
    }

    fn docx(document_xml: &str, extra: &[(&str, &str)]) -> Package {
        let mut parts = vec![
            (
                "[Content_Types].xml".to_string(),
                br#"<?xml version="1.0"?><Types/>"#.to_vec(),
            ),
            (
                "_rels/.rels".to_string(),
                br#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#.to_vec(),
            ),
            ("word/document.xml".to_string(), document_xml.as_bytes().to_vec()),
            (
                "word/styles.xml".to_string(),
                br#"<?xml version="1.0"?><w:styles/>"#.to_vec(),
            ),
        ];
        for (name, xml) in extra {
            parts.push((name.to_string(), xml.as_bytes().to_vec()));
        }
        load_package(&write_zip(&parts)).unwrap()
    }

    #[test]
    fn comment_marker_ids_collects_start_end_and_reference() {
        let doc = parse(
            "<w:p><w:commentRangeStart w:id=\"1\"/><w:r><w:t>a</w:t></w:r>             <w:commentRangeEnd w:id=\"1\"/><w:r><w:commentReference w:id=\"1\"/></w:r></w:p>             <w:commentRangeStart w:id=\"4\"/>             <w:tbl><w:tr><w:tc><w:p><w:hyperlink w:anchor=\"x\"><w:r><w:t>b</w:t></w:r>             <w:commentRangeEnd w:id=\"4\"/></w:hyperlink></w:p></w:tc></w:tr></w:tbl>             <w:p><w:r><w:t>c</w:t><w:commentReference w:id=\"10\"/></w:r></w:p>",
        );
        let ids: Vec<String> = comment_marker_ids(&doc).into_iter().collect();
        assert_eq!(ids, ["1", "10", "4"]);
        assert!(comment_marker_ids(&parse("<w:p><w:r><w:t>x</w:t></w:r></w:p>")).is_empty());
    }

    const VISIBLE: &str = "<w:r><w:t>keep</w:t></w:r>";
    const HIDDEN: &str = "<w:r><w:rPr><w:vanish/></w:rPr><w:t>gone</w:t></w:r>";

    #[test]
    fn inspect_counts_and_removes_hidden_runs() {
        let body = format!(
            "<w:p>{VISIBLE}{HIDDEN}</w:p>\
             <w:tbl><w:tr><w:tc><w:p>{HIDDEN}{VISIBLE}</w:p></w:tc></w:tr></w:tbl>\
             <w:p><w:hyperlink w:anchor=\"a\">{VISIBLE}{HIDDEN}</w:hyperlink></w:p>\
             <w:p><w:hyperlink w:anchor=\"b\">{VISIBLE}<w:ins w:id=\"7\" w:author=\"A\">{HIDDEN}</w:ins></w:hyperlink></w:p>\
             <w:p><w:ins w:id=\"8\" w:author=\"A\">{VISIBLE}{HIDDEN}</w:ins></w:p>"
        );
        let mut doc = parse(&body);
        assert_eq!(count_hidden_runs(&doc), 5);
        assert_eq!(remove_hidden_text(&mut doc), 5);
        assert_eq!(count_hidden_runs(&doc), 0);
        let texts: Vec<String> = doc.body.iter().map(|b| b.plain_text()).collect();
        assert!(texts.iter().all(|t| !t.contains("gone")), "{texts:?}");
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("gone"), "a raw-backed wrapper kept it: {xml}");
        assert!(!xml.contains("w:vanish"), "{xml}");
        assert_eq!(xml.matches("keep").count(), 5, "{xml}");
        // Each touched wrapper is rebuilt rather than written from `raw`.
        for b in &doc.body {
            let Block::Paragraph(p) = b else { continue };
            for inline in &p.content {
                match inline {
                    Inline::Hyperlink(h) => assert!(h.content_changed),
                    Inline::Revision {
                        content_changed, ..
                    } => assert!(content_changed),
                    _ => {}
                }
            }
        }
        assert_eq!(remove_hidden_text(&mut doc), 0);
    }

    #[test]
    fn inspect_web_hidden_is_not_hidden_text() {
        // A Word TOC entry: the tab and PAGEREF page number are webHidden.
        let body = "<w:p><w:hyperlink w:anchor=\"_Toc1\">\
            <w:r><w:t>Intro</w:t></w:r>\
            <w:r><w:rPr><w:webHidden/></w:rPr><w:tab/></w:r>\
            <w:r><w:rPr><w:webHidden/></w:rPr><w:t>3</w:t></w:r>\
            </w:hyperlink></w:p>";
        let mut doc = parse(body);
        let loaded = doc.clone();
        assert_eq!(count_hidden_runs(&doc), 0);
        assert_eq!(remove_hidden_text(&mut doc), 0);
        assert_eq!(doc, loaded);
    }

    #[test]
    fn inspect_counts_vanish_plus_web_hidden_as_hidden_916() {
        // Hidden text that is also webHidden is still hidden text.
        let body = "<w:p><w:r><w:t>keep</w:t></w:r>\
            <w:r><w:rPr><w:vanish/><w:webHidden/></w:rPr><w:t>gone</w:t></w:r></w:p>";
        let mut doc = parse(body);
        assert_eq!(count_hidden_runs(&doc), 1);
        assert_eq!(remove_hidden_text(&mut doc), 1);
        assert_eq!(doc.plain_text().trim(), "keep");
    }

    #[test]
    fn inspect_removes_all_comment_markers_everywhere() {
        let marked = |id: u32| {
            format!(
                "<w:commentRangeStart w:id=\"{id}\"/>{VISIBLE}<w:commentRangeEnd w:id=\"{id}\"/>\
                 <w:r><w:commentReference w:id=\"{id}\"/></w:r>"
            )
        };
        let body = format!(
            "<w:p>{}<w:bookmarkStart w:id=\"0\" w:name=\"bm\"/>{VISIBLE}<w:bookmarkEnd w:id=\"0\"/></w:p>\
             <w:tbl><w:tr><w:tc><w:p>{}</w:p></w:tc></w:tr></w:tbl>\
             <w:p><w:ins w:id=\"9\" w:author=\"A\">{}</w:ins></w:p>\
             <w:p><w:hyperlink w:anchor=\"a\"><w:ins w:id=\"10\" w:author=\"A\">{VISIBLE}</w:ins>{}</w:hyperlink></w:p>",
            marked(1),
            marked(2),
            marked(1),
            marked(2)
        );
        let mut doc = parse(&body);
        assert!(has_comment_markers(&doc));
        assert!(remove_all_comment_markers(&mut doc) >= 8);
        assert!(!has_comment_markers(&doc));
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("comment"), "{xml}");
        assert!(xml.contains("w:bookmarkStart"), "other raw kept: {xml}");
        assert_eq!(xml.matches("keep").count(), 6, "{xml}");
    }

    /// Review r1 C1: a marker shares its raw XML with visible content. Only
    /// the marker goes, through removal and save.
    #[test]
    fn inspect_comment_removal_keeps_text_sharing_raw_with_a_marker() {
        let body = "<w:p><w:r><w:t>hello</w:t><w:commentReference w:id=\"0\"/></w:r></w:p>\
            <w:p><w:customXml w:element=\"note\"><w:commentRangeStart w:id=\"1\"/>\
            <w:r><w:t>Body text</w:t></w:r><w:commentRangeEnd w:id=\"1\"/></w:customXml></w:p>\
            <w:p><w:r><w:rPr><w:rStyle w:val=\"CommentReference\"/></w:rPr>\
            <w:commentReference w:id=\"2\"/></w:r>\
            <w:r><w:t>after</w:t></w:r></w:p>";
        let mut doc = parse(body);
        assert_eq!(count_blocks(&doc.body, Target::CommentMarkers(None)), 4);
        assert_eq!(remove_all_comment_markers(&mut doc), 4);
        assert!(!has_comment_markers(&doc));
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("comment"), "{xml}");
        for kept in ["hello", "Body text", "w:customXml", "after"] {
            assert!(xml.contains(kept), "{kept}: {xml}");
        }
        // The run that held only the reference is gone, not left empty.
        assert!(!xml.contains("CommentReference"), "{xml}");
    }

    /// Review r1 m1: markers in a body-level raw block, inside a move and
    /// inside a field's raw XML are found and removed.
    #[test]
    fn inspect_removes_markers_in_raw_blocks_moves_and_fields() {
        let body = "<w:commentRangeStart w:id=\"0\"/>\
            <w:tbl><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>\
            <w:commentRangeEnd w:id=\"0\"/>\
            <w:p><w:moveTo w:id=\"3\" w:author=\"A\"><w:commentRangeStart w:id=\"1\"/>\
            <w:r><w:t>moved</w:t></w:r><w:commentRangeEnd w:id=\"1\"/></w:moveTo></w:p>\
            <w:p><w:fldSimple w:instr=\" PAGE \"><w:commentRangeStart w:id=\"2\"/>\
            <w:r><w:t>7</w:t></w:r><w:commentRangeEnd w:id=\"2\"/></w:fldSimple></w:p>";
        let mut doc = parse(body);
        // The shapes under test: a raw block, a move record, a field.
        assert!(matches!(&doc.body[0], Block::Raw(r) if r.contains("commentRangeStart")));
        let inlines: Vec<&Inline> = doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => Some(&p.content),
                _ => None,
            })
            .flatten()
            .collect();
        assert!(inlines.iter().any(|i| matches!(
            i,
            Inline::UnsupportedRevision { raw, .. } if raw.contains("commentRangeStart")
        )));
        assert!(inlines.iter().any(|i| matches!(
            i,
            Inline::Field { raw, .. } if raw.contains("commentRangeStart")
        )));
        assert!(has_comment_markers(&doc));
        let found = count_blocks(&doc.body, Target::CommentMarkers(None));
        assert_eq!(remove_all_comment_markers(&mut doc), found);
        assert!(!has_comment_markers(&doc));
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("comment"), "{xml}");
        for kept in ["cell", "moved", "w:moveTo", "PAGE"] {
            assert!(xml.contains(kept), "{kept}: {xml}");
        }
    }

    /// A Word 2010+ text box: a DrawingML choice and a VML fallback, each
    /// with its own copy of the content.
    fn two_copy_text_box(content: &str) -> String {
        format!(
            "<w:p><w:r><mc:AlternateContent><mc:Choice Requires=\"wps\"><w:drawing><wps:txbx>\
             <w:txbxContent>{content}</w:txbxContent></wps:txbx></w:drawing></mc:Choice>\
             <mc:Fallback><w:pict><v:shape><v:textbox><w:txbxContent>{content}</w:txbxContent>\
             </v:textbox></v:shape></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>"
        )
    }

    /// Review r1 m2: removal reaches the fallback copy of a text box too.
    #[test]
    fn inspect_removal_reaches_a_text_box_fallback_copy() {
        let hidden = two_copy_text_box(&format!("<w:p>{VISIBLE}{HIDDEN}</w:p>"));
        let mut doc = parse(&hidden);
        assert_eq!(count_hidden_runs(&doc), 1);
        assert_eq!(remove_hidden_text(&mut doc), 1);
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("gone"), "{xml}");
        assert_eq!(xml.matches("keep").count(), 2, "both copies keep: {xml}");
        assert!(xml.contains("<mc:Fallback>"), "{xml}");

        let marked = two_copy_text_box(&format!(
            "<w:p><w:commentRangeStart w:id=\"4\"/>{VISIBLE}<w:commentRangeEnd w:id=\"4\"/></w:p>"
        ));
        let mut doc = parse(&marked);
        // Two markers, written twice: the twin copy is not counted again.
        assert_eq!(count_blocks(&doc.body, Target::CommentMarkers(None)), 2);
        assert_eq!(remove_all_comment_markers(&mut doc), 2);
        assert!(!has_comment_markers(&doc));
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("comment"), "{xml}");
        assert_eq!(xml.matches("keep").count(), 2, "{xml}");
    }

    /// A group shape with boxes A and B: the choice holds A then B, the
    /// fallback A' then B'. The loader reads A into `blocks`.
    fn group_of_two_boxes(a: &str, b: &str) -> String {
        format!(
            "<w:p><w:r><mc:AlternateContent><mc:Choice Requires=\"wpg\"><w:drawing><wpg:wgp>\
             <wps:wsp><wps:txbx><w:txbxContent>{a}</w:txbxContent></wps:txbx></wps:wsp>\
             <wps:wsp><wps:txbx><w:txbxContent>{b}</w:txbxContent></wps:txbx></wps:wsp>\
             </wpg:wgp></w:drawing></mc:Choice><mc:Fallback><w:pict><v:group>\
             <v:shape><v:textbox><w:txbxContent>{a}</w:txbxContent></v:textbox></v:shape>\
             <v:shape><v:textbox><w:txbxContent>{b}</w:txbxContent></v:textbox></v:shape>\
             </v:group></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>"
        )
    }

    /// Review r2 C1: syncing the fallback rewrites A's twin only, never the
    /// group's other box.
    #[test]
    fn inspect_text_box_sync_spares_a_group_s_other_box() {
        let a = format!("<w:p>{VISIBLE}{HIDDEN}</w:p>");
        let b = "<w:p><w:r><w:t>Box B</w:t></w:r></w:p>";
        let mut doc = parse(&group_of_two_boxes(&a, b));
        assert_eq!(count_hidden_runs(&doc), 1);
        assert_eq!(remove_hidden_text(&mut doc), 1);
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("gone"), "A and its twin lose it: {xml}");
        assert_eq!(xml.matches("keep").count(), 2, "{xml}");
        assert_eq!(xml.matches("Box B").count(), 2, "B kept in both: {xml}");

        // B's markers live only in raw: found, removed, counted once each.
        let b = "<w:p><w:commentRangeStart w:id=\"8\"/><w:r><w:t>Box B</w:t></w:r>\
                 <w:commentRangeEnd w:id=\"8\"/></w:p>";
        let mut doc = parse(&group_of_two_boxes(&format!("<w:p>{VISIBLE}</w:p>"), b));
        assert!(has_comment_markers(&doc));
        assert_eq!(count_blocks(&doc.body, Target::CommentMarkers(None)), 4);
        assert_eq!(remove_all_comment_markers(&mut doc), 4);
        assert!(!has_comment_markers(&doc));
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("comment"), "{xml}");
        assert_eq!(xml.matches("Box B").count(), 2, "{xml}");
        assert_eq!(xml.matches("keep").count(), 2, "{xml}");
    }

    /// A two-copy text box whose paragraph carries Word's rsids, a comment
    /// range and a hidden run.
    fn rsid_box() -> String {
        two_copy_text_box(&format!(
            "<w:p w:rsidR=\"00A1B2C3\" w:rsidRDefault=\"00A1B2C3\">\
             <w:commentRangeStart w:id=\"5\"/>{VISIBLE}<w:commentRangeEnd w:id=\"5\"/>{HIDDEN}</w:p>"
        ))
    }

    /// Review r3 M1: after one Remove All rewrites the copies, a second still
    /// sees them as twins, in either order.
    #[test]
    fn inspect_second_removal_still_reaches_the_fallback() {
        // Comments, then hidden text.
        let mut doc = parse(&rsid_box());
        assert_eq!(remove_all_comment_markers(&mut doc), 1 + 1);
        assert_eq!(count_hidden_runs(&doc), 1);
        assert_eq!(remove_hidden_text(&mut doc), 1);
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("gone"), "{xml}");
        assert!(!xml.contains("comment"), "{xml}");
        assert_eq!(xml.matches("keep").count(), 2, "{xml}");

        // Hidden text, then comments: the second pass counts what it removes.
        let mut doc = parse(&rsid_box());
        assert_eq!(remove_hidden_text(&mut doc), 1);
        let markers = count_blocks(&doc.body, Target::CommentMarkers(None));
        assert_eq!(markers, 2);
        assert_eq!(remove_all_comment_markers(&mut doc), markers);
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("gone"), "{xml}");
        assert!(!xml.contains("comment"), "{xml}");
        assert_eq!(xml.matches("keep").count(), 2, "{xml}");
    }

    /// #1063: text-box paragraphs keep their `w14:paraId`, the same in the
    /// Choice and the Fallback copy. A save must not drop it from the Fallback
    /// as a repeat, or the reopened copies differ and a second removal no
    /// longer reaches the Fallback.
    #[test]
    fn inspect_removal_after_reopen_reaches_a_fallback_with_para_ids() {
        let boxed = two_copy_text_box(&format!(
            "<w:p w14:paraId=\"1A2B3C4D\" w:rsidR=\"00A1B2C3\">\
             <w:commentRangeStart w:id=\"5\"/>{VISIBLE}<w:commentRangeEnd w:id=\"5\"/>{HIDDEN}</w:p>"
        ));
        let mut doc = parse(&boxed);
        assert_eq!(remove_all_comment_markers(&mut doc), 1 + 1);
        let xml = document_to_xml(&doc);
        assert_eq!(xml.matches("w14:paraId=\"1A2B3C4D\"").count(), 2, "{xml}");
        let body = &xml[xml.find("<w:body>").unwrap() + 8..xml.find("</w:body>").unwrap()];
        let mut reopened = parse(body);
        assert_eq!(remove_hidden_text(&mut reopened), 1);
        let xml = document_to_xml(&reopened);
        assert!(!xml.contains("gone"), "{xml}");
        assert_eq!(xml.matches("keep").count(), 2, "{xml}");
        assert_eq!(xml.matches("w14:paraId=\"1A2B3C4D\"").count(), 2, "{xml}");
    }

    /// Review r2: a duplicated text box (a copy, a mail-merge record) repeats
    /// its paragraph ids. After an Inspect pass syncs the copies, the save
    /// drops the repeat from the second box's Choice but never from a
    /// Fallback, so its copies differ by an id; they are still twins after a
    /// reopen, and a removal reaches both.
    #[test]
    fn inspect_removal_reaches_both_copies_of_a_duplicated_text_box() {
        let boxed = two_copy_text_box(&format!(
            "<w:p w14:paraId=\"1A2B3C4D\">\
             <w:commentRangeStart w:id=\"5\"/>{VISIBLE}<w:commentRangeEnd w:id=\"5\"/>{HIDDEN}</w:p>"
        ));
        let mut doc = parse(&format!("{boxed}{boxed}"));
        assert_eq!(remove_all_comment_markers(&mut doc), 4);
        let xml = document_to_xml(&doc);
        assert_eq!(xml.matches("w14:paraId=\"1A2B3C4D\"").count(), 3, "{xml}");
        let body = &xml[xml.find("<w:body>").unwrap() + 8..xml.find("</w:body>").unwrap()];
        let mut reopened = parse(body);
        assert_eq!(remove_hidden_text(&mut reopened), 2);
        let xml = document_to_xml(&reopened);
        assert!(!xml.contains("gone"), "{xml}");
        assert_eq!(xml.matches("keep").count(), 4, "{xml}");
    }

    /// Review r3: copies are twins apart from `w14:paraId` attributes only,
    /// never apart from text that happens to look like one.
    #[test]
    fn text_box_twins_ignore_para_id_attributes_but_not_text() {
        let raw = |a: &str, b: &str| {
            format!(
                "<v:group><w:txbxContent>{a}</w:txbxContent><w:txbxContent>{b}</w:txbxContent></v:group>"
            )
        };
        let text =
            |id: &str| format!("<w:p><w:r><w:t>Example w14:paraId=\"{id}\"</w:t></w:r></w:p>");
        let attr = |id: &str| format!("<w:p w14:paraId=\"{id}\"><w:r><w:t>Same</w:t></w:r></w:p>");
        let twins = |raw: &str| {
            text_box_copies(raw)
                .iter()
                .map(|c| c.twin)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            twins(&raw(&text("11111111"), &text("22222222"))),
            [true, false]
        );
        assert_eq!(
            twins(&raw(&attr("11111111"), &attr("22222222"))),
            [true, true]
        );
    }

    /// #917: deleting one comment strips only its markers. Text in the same
    /// raw run and a `w:customXml` wrapper around its range stay; a run left
    /// holding only `w:rPr` goes; the other comments' markers stay.
    #[test]
    fn remove_one_comment_keeps_text_sharing_raw_with_its_marker_917() {
        let body = "<w:p><w:r><w:t>hello</w:t><w:commentReference w:id=\"0\"/></w:r></w:p>\
            <w:p><w:customXml w:element=\"note\"><w:commentRangeStart w:id=\"1\"/>\
            <w:r><w:t>Body text</w:t></w:r><w:commentRangeEnd w:id=\"1\"/></w:customXml></w:p>\
            <w:p><w:r><w:rPr><w:rStyle w:val=\"CommentReference\"/></w:rPr>\
            <w:commentReference w:id=\"2\"/></w:r>\
            <w:r><w:t>after</w:t></w:r></w:p>";
        let mut doc = parse(body);
        assert_eq!(remove_comment_markers(&mut doc, "0"), 1);
        let xml = document_to_xml(&doc);
        assert!(xml.contains("hello"), "{xml}");
        assert_eq!(count_markers(&xml, Some("0")), 0, "{xml}");
        assert_eq!(count_markers(&xml, None), 3, "others kept: {xml}");

        assert_eq!(remove_comment_markers(&mut doc, "1"), 2);
        let xml = document_to_xml(&doc);
        assert!(xml.contains("w:customXml"), "{xml}");
        assert!(xml.contains("Body text"), "{xml}");
        assert_eq!(count_markers(&xml, Some("1")), 0, "{xml}");
        assert_eq!(count_markers(&xml, Some("2")), 1, "{xml}");

        assert_eq!(remove_comment_markers(&mut doc, "2"), 1);
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("comment"), "{xml}");
        // The run that held only the reference is gone, not saved empty.
        assert!(!xml.contains("CommentReference"), "{xml}");
        for kept in ["hello", "Body text", "w:customXml", "after"] {
            assert!(xml.contains(kept), "{kept}: {xml}");
        }
        assert_eq!(remove_comment_markers(&mut doc, "2"), 0);
    }

    /// #917: one comment's markers are removed from every place the walk
    /// reaches (hyperlink, revision, table cell, field, move, body-level raw
    /// block, both copies of a text box); comment 10's stay everywhere.
    #[test]
    fn remove_one_comment_reaches_every_container_917() {
        let marked = |inner: &str| {
            format!(
                "<w:commentRangeStart w:id=\"1\"/><w:commentRangeStart w:id=\"10\"/>{inner}\
                 <w:commentRangeEnd w:id=\"1\"/><w:commentRangeEnd w:id=\"10\"/>\
                 <w:r><w:commentReference w:id=\"1\"/></w:r>\
                 <w:r><w:commentReference w:id=\"10\"/></w:r>"
            )
        };
        let body = format!(
            "<w:commentRangeStart w:id=\"1\"/><w:commentRangeStart w:id=\"10\"/>\
             <w:tbl><w:tr><w:tc><w:p>{}</w:p></w:tc></w:tr></w:tbl>\
             <w:commentRangeEnd w:id=\"1\"/><w:commentRangeEnd w:id=\"10\"/>\
             <w:p><w:hyperlink w:anchor=\"a\">{}</w:hyperlink></w:p>\
             <w:p><w:ins w:id=\"9\" w:author=\"A\">{}</w:ins></w:p>\
             <w:p><w:moveTo w:id=\"3\" w:author=\"A\">{}</w:moveTo></w:p>\
             <w:p><w:fldSimple w:instr=\" PAGE \">{}</w:fldSimple></w:p>\
             {}",
            marked("<w:r><w:t>cell</w:t></w:r>"),
            marked("<w:r><w:t>link</w:t></w:r>"),
            marked("<w:r><w:t>inserted</w:t></w:r>"),
            marked("<w:r><w:t>moved</w:t></w:r>"),
            marked("<w:r><w:t>7</w:t></w:r>"),
            two_copy_text_box(&format!("<w:p>{}</w:p>", marked(VISIBLE))),
        );
        let mut doc = parse(&body);
        // The shapes under test.
        assert!(matches!(&doc.body[0], Block::Raw(r) if r.contains("commentRangeStart")));
        let inlines: Vec<&Inline> = doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => Some(&p.content),
                _ => None,
            })
            .flatten()
            .collect();
        for shape in [
            "Hyperlink",
            "Revision",
            "UnsupportedRevision",
            "Field",
            "TextBox",
        ] {
            assert!(
                inlines.iter().any(|i| format!("{i:?}").starts_with(shape)),
                "{shape}: {inlines:?}"
            );
        }
        let before = document_to_xml(&doc);
        let ones = count_blocks(&doc.body, Target::CommentMarkers(Some("1")));
        let tens = count_blocks(&doc.body, Target::CommentMarkers(Some("10")));
        // 6 containers + the raw blocks' 2, the text box counted once.
        assert_eq!(ones, 6 * 3 + 2, "{before}");
        assert_eq!(tens, ones);

        assert_eq!(remove_comment_markers(&mut doc, "1"), ones);
        assert_eq!(
            count_blocks(&doc.body, Target::CommentMarkers(Some("1"))),
            0
        );
        assert_eq!(
            count_blocks(&doc.body, Target::CommentMarkers(Some("10"))),
            tens
        );
        let xml = document_to_xml(&doc);
        assert_eq!(count_markers(&xml, Some("1")), 0, "{xml}");
        assert_eq!(
            count_markers(&xml, Some("10")),
            count_markers(&before, Some("10")),
            "{xml}"
        );
        for kept in [
            "cell",
            "link",
            "inserted",
            "moved",
            "w:moveTo",
            "PAGE",
            "<mc:Fallback>",
        ] {
            assert!(xml.contains(kept), "{kept}: {xml}");
        }
        assert_eq!(xml.matches("keep").count(), 2, "both box copies: {xml}");
    }

    /// #917: `w:id` is compared whole, whatever the attribute order, quote
    /// style or element form.
    #[test]
    fn remove_one_comment_matches_the_id_exactly_917() {
        let xml = "<w:commentRangeStart w:id=\"10\"/><w:commentRangeStart w:id=\"11\"/>\
            <w:commentRangeStart w:id=\"21\"/><w:commentRangeStart w:id='1'/>\
            <w:commentRangeEnd w:displacedByCustomXml=\"prev\" w:id=\"1\"/>\
            <w:commentRangeEnd w:id=\"1\" w:displacedByCustomXml=\"next\"/>\
            <w:commentRangeStart w:id=\"1\"></w:commentRangeStart>\
            <w:r><w:commentReference w:id=\"1\" /></w:r>\
            <w:bookmarkStart w:id=\"1\" w:name=\"b\"/>";
        assert_eq!(count_markers(xml, Some("1")), 5);
        let (out, n) = strip_markers(xml, Some("1"));
        assert_eq!(n, 5);
        assert_eq!(
            out,
            "<w:commentRangeStart w:id=\"10\"/><w:commentRangeStart w:id=\"11\"/>\
             <w:commentRangeStart w:id=\"21\"/><w:r></w:r>\
             <w:bookmarkStart w:id=\"1\" w:name=\"b\"/>"
        );
        assert_eq!(strip_markers(xml, Some("2")).1, 0);
        assert_eq!(strip_markers(xml, None).1, 8);

        // Through a document: comment 1's markers go, 10's, 11's and 21's stay.
        let mut doc = parse(&format!("<w:p>{xml}<w:r><w:t>x</w:t></w:r></w:p>"));
        assert_eq!(remove_comment_markers(&mut doc, "1"), 5);
        let saved = document_to_xml(&doc);
        assert_eq!(count_markers(&saved, Some("1")), 0, "{saved}");
        for other in ["10", "11", "21"] {
            assert_eq!(count_markers(&saved, Some(other)), 1, "{other}: {saved}");
        }
        assert!(saved.contains("w:bookmarkStart"), "{saved}");
    }

    /// Review r3 M2: hidden runs that stay raw XML are counted (not removed).
    #[test]
    fn inspect_counts_hidden_runs_left_in_raw_xml() {
        let body = format!(
            "<w:p><w:moveTo w:id=\"3\" w:author=\"A\">{HIDDEN}{VISIBLE}</w:moveTo></w:p>\
             <w:p><w:fldSimple w:instr=\" XE &quot;term&quot; \">{HIDDEN}</w:fldSimple></w:p>\
             {}",
            group_of_two_boxes(
                &format!("<w:p>{VISIBLE}</w:p>"),
                &format!("<w:p>{HIDDEN}</w:p>")
            )
        );
        let mut doc = parse(&body);
        assert_eq!(count_hidden_runs(&doc), 0, "none of it is modeled");
        // One in the move, one in the field, B and its fallback twin B'.
        assert_eq!(count_unremovable_hidden_runs(&doc), 4);
        assert_eq!(remove_hidden_text(&mut doc), 0);
        assert_eq!(count_unremovable_hidden_runs(&doc), 4, "left in place");
    }

    #[test]
    fn hidden_in_xml_counts_leaf_runs_by_their_own_props() {
        let count = count_hidden_in_xml;
        assert_eq!(count(HIDDEN), 1);
        assert_eq!(count(VISIBLE), 0);
        assert_eq!(
            count("<w:r><w:rPr><w:vanish w:val=\"0\"/></w:rPr><w:t>x</w:t></w:r>"),
            0
        );
        assert_eq!(
            count("<w:r><w:rPr><w:vanish w:val=\"false\"/></w:rPr><w:t>x</w:t></w:r>"),
            0
        );
        // A TOC page number: webHidden, not hidden text.
        assert_eq!(
            count("<w:r><w:rPr><w:webHidden/></w:rPr><w:t>3</w:t></w:r>"),
            0
        );
        // Hidden text that is also webHidden is hidden, as in the model.
        assert_eq!(
            count("<w:r><w:rPr><w:vanish/><w:webHidden/></w:rPr><w:t>3</w:t></w:r>"),
            1
        );
        // The previous formatting of a tracked change is not the run's own.
        assert_eq!(
            count(
                "<w:r><w:rPr><w:b/><w:rPrChange w:id=\"1\"><w:rPr><w:vanish/></w:rPr>\
                 </w:rPrChange></w:rPr><w:t>x</w:t></w:r>"
            ),
            0
        );
        // A drawing run holding a text box's runs: only the inner run counts.
        assert_eq!(
            count(&format!(
                "<w:r><w:rPr><w:vanish/></w:rPr><w:pict><w:txbxContent><w:p>{HIDDEN}</w:p>\
                 </w:txbxContent></w:pict></w:r>"
            )),
            1
        );
    }

    #[test]
    fn leftover_is_only_nothing_or_a_bare_run() {
        assert!(is_leftover("  "));
        assert!(is_leftover("<w:r></w:r>"));
        assert!(is_leftover("<w:r><w:rPr><w:b/></w:rPr></w:r>"));
        assert!(!is_leftover("<w:r><w:t>x</w:t></w:r>"));
        assert!(!is_leftover("<w:r></w:r><w:r><w:t>x</w:t></w:r>"));
        assert!(!is_leftover("<w:customXml w:element=\"e\"></w:customXml>"));
    }

    #[test]
    fn inspect_removes_personal_core_and_app_properties() {
        let core = "<?xml version=\"1.0\"?><cp:coreProperties xmlns:cp=\"c\" xmlns:dc=\"d\" xmlns:dcterms=\"t\">\
            <dc:title xml:lang=\"en\">Plan</dc:title><dc:creator>Ann</dc:creator>\
            <cp:lastModifiedBy>Bob</cp:lastModifiedBy><cp:keywords/>\
            <cp:revision>4</cp:revision>\
            <dcterms:created xsi:type=\"dcterms:W3CDTF\">2026-01-01T00:00:00Z</dcterms:created>\
            </cp:coreProperties>";
        let app = "<Properties xmlns=\"p\"><Application>Microsoft Office Word</Application>\
            <Company>Acme</Company><Manager/></Properties>";
        let mut pkg = docx(
            &format!("<w:document {W}><w:body><w:p/></w:body></w:document>"),
            &[("docProps/core.xml", core), ("docProps/app.xml", app)],
        );
        assert!(has_personal_properties(&pkg));
        assert!(remove_personal_properties(&mut pkg));
        assert!(!has_personal_properties(&pkg));
        let core = pkg.part_text("docProps/core.xml").unwrap();
        for gone in ["dc:title", "dc:creator", "cp:lastModifiedBy", "cp:keywords"] {
            assert!(!core.contains(gone), "{gone}: {core}");
        }
        assert!(core.contains("<cp:revision>4</cp:revision>"), "{core}");
        assert!(core.contains("<dcterms:created"), "{core}");
        let app = pkg.part_text("docProps/app.xml").unwrap();
        assert_eq!(
            app,
            "<Properties xmlns=\"p\"><Application>Microsoft Office Word</Application></Properties>"
        );
        assert!(!remove_personal_properties(&mut pkg), "nothing left");
    }

    #[test]
    fn inspect_empty_properties_not_found() {
        let core = "<cp:coreProperties xmlns:cp=\"c\" xmlns:dc=\"d\"><dc:title/>\
            <dc:creator></dc:creator><dc:subject>  </dc:subject><dc:titleX>t</dc:titleX></cp:coreProperties>";
        let pkg = docx(
            &format!("<w:document {W}><w:body><w:p/></w:body></w:document>"),
            &[("docProps/core.xml", core)],
        );
        assert!(!has_personal_properties(&pkg));
    }

    #[test]
    fn inspect_custom_properties_emptied() {
        let custom = "<Properties xmlns=\"c\" xmlns:vt=\"v\">\
            <property fmtid=\"{D5CDD505-2E9C-101B-9397-08002B2CF9AE}\" pid=\"2\" name=\"Client\"><vt:lpwstr>Acme</vt:lpwstr></property>\
            <property fmtid=\"{D5CDD505-2E9C-101B-9397-08002B2CF9AE}\" pid=\"3\" name=\"Ref\"><vt:i4>7</vt:i4></property>\
            </Properties>";
        let mut pkg = docx(
            &format!("<w:document {W}><w:body><w:p/></w:body></w:document>"),
            &[("docProps/custom.xml", custom)],
        );
        assert!(has_personal_properties(&pkg));
        assert!(remove_personal_properties(&mut pkg));
        assert_eq!(
            pkg.part_text("docProps/custom.xml").unwrap(),
            "<Properties xmlns=\"c\" xmlns:vt=\"v\"></Properties>"
        );
        assert!(!has_personal_properties(&pkg));
    }

    #[test]
    fn no_property_parts_is_not_found() {
        let mut pkg = docx(
            &format!("<w:document {W}><w:body><w:p/></w:body></w:document>"),
            &[],
        );
        assert!(!has_personal_properties(&pkg));
        assert!(!remove_personal_properties(&mut pkg));
    }
}
