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

use crate::model::{Block, Document, Inline, Run, RunProps, Table, TableRowBoundaryKind};
use crate::package::Package;
use crate::styles::StyleSheet;
use crate::xml::{Event, XmlParser};
use std::ops::Range;

/// The styles a [`Target::Hidden`] walk resolves `w:vanish` through, and the
/// style of the paragraph it is in.
#[derive(Clone, Copy)]
struct Styled<'a> {
    sheet: &'a StyleSheet,
    para: Option<&'a str>,
}

/// What a body walk removes or counts.
#[derive(Clone, Copy)]
enum Target<'a> {
    /// Runs, tabs and breaks formatted `w:vanish`: directly, or through
    /// their styles when the walk has them.
    Hidden(Option<Styled<'a>>),
    /// `w:commentRangeStart`, `w:commentRangeEnd` and `w:commentReference`:
    /// every comment's, or only those whose `w:id` is the given one.
    CommentMarkers(Option<&'a str>),
    /// Hidden runs inside raw XML ([`count_hidden_in_xml`]). Counted only.
    RawHidden,
}

impl<'a> Target<'a> {
    /// Whether the walk drops this modeled inline whole. Comment markers
    /// live in raw XML and are stripped from it instead ([`strip_markers`]).
    fn matches(self, inline: &Inline) -> bool {
        match (self, inline) {
            (Target::Hidden(styled), Inline::Run(r)) => is_hidden(&r.props, styled),
            (Target::Hidden(styled), Inline::Tab(p) | Inline::Break(_, p)) => is_hidden(p, styled),
            _ => false,
        }
    }

    fn matches_run(self, run: &Run) -> bool {
        matches!(self, Target::Hidden(styled) if is_hidden(&run.props, styled))
    }

    /// This target inside a paragraph of style `para`.
    fn in_paragraph<'b>(self, para: Option<&'b str>) -> Target<'b>
    where
        'a: 'b,
    {
        match self {
            Target::Hidden(Some(styled)) => Target::Hidden(Some(Styled {
                sheet: styled.sheet,
                para,
            })),
            other => other,
        }
    }
}

/// Whether run formatting hides the text the way Word's inspector means it:
/// `w:vanish`, set directly or, with `styled`, through the run's character
/// style, its paragraph's style or the document defaults
/// ([`StyleSheet::effective_vanish`]). `w:webHidden` (hidden only in Web
/// layout; every TOC puts it on its page numbers) is not hidden text and is
/// not `vanish`.
fn is_hidden(props: &RunProps, styled: Option<Styled>) -> bool {
    match styled {
        None => props.vanish,
        Some(s) => s
            .sheet
            .effective_vanish(s.para, props.style_id.as_deref(), props),
    }
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
        Target::Hidden(_) => 0,
    }
}

fn count_blocks(blocks: &[Block], target: Target) -> usize {
    blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => {
                count_inlines(&p.content, target.in_paragraph(p.props.style_id.as_deref()))
            }
            Block::Table(t) => {
                let cells: usize = t
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .map(|cell| count_blocks(&cell.blocks, target))
                    .sum();
                cells
                    + table_raws(t)
                        .map(|raw| count_in_raw(raw, target))
                        .sum::<usize>()
            }
            Block::Raw(raw) => count_in_raw(raw, target),
            Block::SectionProperties(_) => 0,
        })
        .sum()
}

/// The raw XML of an inline that [`count_inlines`] counts in, for any
/// target. Hyperlinks and revisions are not here: their children are walked,
/// and a changed wrapper is rebuilt from them. Text boxes are not either:
/// their raw holds copies of their blocks ([`text_box_extra`]). Comment
/// markers are read from more inlines: [`comment_marker_raw`].
fn marker_raw(inline: &Inline) -> Option<&str> {
    match inline {
        Inline::Raw(raw) | Inline::Field { raw, .. } | Inline::UnsupportedRevision { raw, .. } => {
            Some(raw)
        }
        _ => None,
    }
}

/// The raw XML comment markers are read from: [`marker_raw`]'s, and the run
/// of a footnote or endnote reference, chart, SmartArt or equation, each
/// kept whole as its raw and written back verbatim, so a
/// `w:commentReference` sharing that run sits there (#1107).
fn comment_marker_raw(inline: &Inline) -> Option<&str> {
    match inline {
        Inline::FootnoteRef { raw, .. }
        | Inline::Chart { raw, .. }
        | Inline::SmartArt { raw, .. }
        | Inline::Equation { raw, .. } => Some(raw),
        _ => marker_raw(inline),
    }
}

/// [`comment_marker_raw`], to strip markers from.
fn marker_raw_mut(inline: &mut Inline) -> Option<&mut String> {
    match inline {
        Inline::Raw(raw)
        | Inline::Field { raw, .. }
        | Inline::UnsupportedRevision { raw, .. }
        | Inline::FootnoteRef { raw, .. }
        | Inline::Chart { raw, .. }
        | Inline::SmartArt { raw, .. }
        | Inline::Equation { raw, .. } => Some(raw),
        _ => None,
    }
}

fn count_inlines(content: &[Inline], target: Target) -> usize {
    content
        .iter()
        .map(|inline| {
            let raw = match target {
                Target::CommentMarkers(_) => comment_marker_raw(inline),
                _ => marker_raw(inline),
            };
            let own = match raw {
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
            Block::Paragraph(p) => {
                let target = target.in_paragraph(p.props.style_id.as_deref());
                removed += remove_inlines(&mut p.content, target);
            }
            Block::Table(t) => {
                for cell in t.rows.iter_mut().flat_map(|row| &mut row.cells) {
                    removed += remove_blocks(&mut cell.blocks, target);
                }
                if let Target::CommentMarkers(id) = target {
                    removed += remove_table_raw_markers(t, id);
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

/// A table's own raw XML outside its cells' blocks: the children between
/// rows ([`TableRowBoundaryKind::Raw`]) and the row-level ones between cells
/// ([`Cell::raw_before`], [`Row::raw_end`]), where a comment range can start
/// or end (#1215).
///
/// [`TableRowBoundaryKind::Raw`]: crate::model::TableRowBoundaryKind::Raw
/// [`Cell::raw_before`]: crate::model::Cell::raw_before
/// [`Row::raw_end`]: crate::model::Row::raw_end
fn table_raws(t: &Table) -> impl Iterator<Item = &String> {
    let between_rows = t.row_boundaries.iter().filter_map(|b| match &b.kind {
        TableRowBoundaryKind::Raw(raw) => Some(raw),
        _ => None,
    });
    let in_rows = t.rows.iter().flat_map(|row| {
        row.cells
            .iter()
            .flat_map(|cell| &cell.raw_before)
            .chain(&row.raw_end)
    });
    between_rows.chain(in_rows)
}

/// Remove the comment markers (of `id`, or all) from [`table_raws`],
/// dropping an entry left empty. Returns how many.
fn remove_table_raw_markers(t: &mut Table, id: Option<&str>) -> usize {
    let mut removed = 0;
    let mut strip = |raw: &mut String| {
        let (out, n) = strip_markers(raw, id);
        if n > 0 {
            *raw = out;
            removed += n;
        }
        !raw.trim().is_empty()
    };
    t.row_boundaries.retain_mut(|b| match &mut b.kind {
        TableRowBoundaryKind::Raw(raw) => strip(raw),
        _ => true,
    });
    for row in &mut t.rows {
        for cell in &mut row.cells {
            cell.raw_before.retain_mut(&mut strip);
        }
        row.raw_end.retain_mut(&mut strip);
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
    /// attributes and prefixed namespace declarations (see `twin_key`), so it
    /// shows the same `blocks`.
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
    let first = twin_key(first);
    inners
        .iter()
        .map(|inner| TextBoxCopy {
            twin: twin_key(&raw[inner.clone()]) == first,
            inner: inner.clone(),
        })
        .collect()
}

/// `xml` as text-box copies are compared: without the `w14:paraId`
/// attributes and the prefixed namespace declarations (`xmlns:p`) of its
/// start tags; text is left alone. A save may drop a repeated id from one copy
/// only, and the loader declares on a modeled copy's property elements the
/// prefixes they inherit (#1063), which the raw copy inherits unchanged.
/// Malformed XML is returned as it is.
fn twin_key(xml: &str) -> String {
    use crate::xml::{Event, XmlParser};
    let mut cuts = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => {
                for a in p
                    .attrs()
                    .iter()
                    .filter(|a| a.name == "w14:paraId" || a.name.starts_with("xmlns:"))
                {
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
/// the ones [`remove_hidden_text`] removes. With `styles`, text hidden
/// through its styles counts too; without, only direct `w:vanish`.
pub fn count_hidden_runs(doc: &Document, styles: Option<&StyleSheet>) -> usize {
    count_blocks(&doc.body, hidden(styles))
}

/// The [`Target::Hidden`] walk over `styles`, if any.
fn hidden(styles: Option<&StyleSheet>) -> Target<'_> {
    Target::Hidden(styles.map(|sheet| Styled { sheet, para: None }))
}

/// Hidden runs that stay raw XML (fields, tracked moves, unmodeled
/// elements, a group shape's other text boxes): found, but not removed by
/// [`remove_hidden_text`].
pub fn count_unremovable_hidden_runs(doc: &Document) -> usize {
    count_blocks(&doc.body, Target::RawHidden)
}

/// Remove every hidden run, tab and break from the modeled body (see
/// [`count_hidden_runs`]). Returns how many.
pub fn remove_hidden_text(doc: &mut Document, styles: Option<&StyleSheet>) -> usize {
    remove_blocks(&mut doc.body, hidden(styles))
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
                for raw in table_raws(t) {
                    marker_ids_in_xml(raw, ids);
                }
            }
            Block::Raw(raw) => marker_ids_in_xml(raw, ids),
            Block::SectionProperties(_) => {}
        }
    }
}

fn marker_ids_in_inlines(content: &[Inline], ids: &mut std::collections::BTreeSet<String>) {
    for inline in content {
        if let Some(raw) = comment_marker_raw(inline) {
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

/// What the inspector finds in a document's other stories ([`Stories`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoryCounts {
    /// Hidden runs, tabs and breaks [`Stories::remove_hidden_text`] removes.
    pub hidden: usize,
    /// Hidden runs left in raw XML there ([`count_unremovable_hidden_runs`]).
    pub hidden_unremovable: usize,
    /// Comment markers (range start/end and references).
    pub comment_markers: usize,
}

impl std::ops::AddAssign for StoryCounts {
    fn add_assign(&mut self, o: StoryCounts) {
        self.hidden += o.hidden;
        self.hidden_unremovable += o.hidden_unremovable;
        self.comment_markers += o.comment_markers;
    }
}

/// The notes parts and the element each note is.
const NOTE_PARTS: [(&str, &str); 2] = [
    ("word/footnotes.xml", "w:footnote"),
    ("word/endnotes.xml", "w:endnote"),
];

/// What a story part holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoryKind {
    /// A header (`true`) or footer part.
    HeaderFooter(bool),
    /// A notes part, whose notes are this element.
    Notes(&'static str),
}

/// A document's other stories (#918): the header and footer parts some
/// section of the body references, of any variant (shown or not: a save
/// keeps them all), then `word/footnotes.xml` and `word/endnotes.xml` when
/// present. A header or footer part no section references any more (kept in
/// the package for undo, dropped by the save, #771) is not one of them.
pub struct Stories {
    parts: Vec<(String, StoryKind)>,
}

impl Stories {
    /// The stories of `pkg` whose body is `doc` (the live one, whose section
    /// breaks say which headers and footers apply).
    pub fn new(pkg: &Package, doc: &Document) -> Stories {
        let mut sect_prs: Vec<&str> = doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => p.props.section_break.as_deref(),
                _ => None,
            })
            .collect();
        sect_prs.push(
            doc.trailing_section_properties()
                .map_or("", |s| s.raw.as_str()),
        );
        let mut parts: Vec<(String, StoryKind)> = Vec::new();
        for section in crate::package::section_header_parts(&sect_prs, &pkg.document_rels()) {
            for (is_header, slots) in [(true, section.headers), (false, section.footers)] {
                for part in slots.into_iter().flatten() {
                    if pkg.part(&part.part_name).is_some()
                        && !parts.iter().any(|(name, _)| *name == part.part_name)
                    {
                        parts.push((part.part_name, StoryKind::HeaderFooter(is_header)));
                    }
                }
            }
        }
        parts.extend(
            NOTE_PARTS
                .iter()
                .filter(|(name, _)| pkg.part(name).is_some())
                .map(|(name, element)| (name.to_string(), StoryKind::Notes(element))),
        );
        Stories { parts }
    }

    /// The story part names, in order.
    pub fn part_names(&self) -> Vec<&str> {
        self.parts.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// Every story's blocks, read from `pkg`, but `skip`'s (a header or footer
    /// whose live content the caller holds, in an editor).
    fn blocks(&self, pkg: &Package, skip: Option<&str>) -> Vec<Vec<Block>> {
        let mut out = Vec::new();
        for (part, kind) in &self.parts {
            if Some(part.as_str()) == skip {
                continue;
            }
            match *kind {
                StoryKind::HeaderFooter(_) => out.extend(pkg.header_footer_blocks(part)),
                StoryKind::Notes(element) => {
                    let Some(xml) = pkg.part_text(part) else {
                        continue;
                    };
                    let rels = pkg.part_rels(part);
                    for range in note_ranges(&xml, element) {
                        out.push(crate::load::parse_note_blocks(&xml[range], &rels));
                    }
                }
            }
        }
        out
    }

    /// What the inspector finds in the stories, hidden text resolved through
    /// `styles` when given. `skip` names a header or footer part to leave
    /// out: one open in an editor, whose live blocks the caller counts with
    /// [`count_story_blocks`].
    pub fn count(
        &self,
        pkg: &Package,
        styles: Option<&StyleSheet>,
        skip: Option<&str>,
    ) -> StoryCounts {
        let mut counts = StoryCounts::default();
        for blocks in self.blocks(pkg, skip) {
            counts += count_story_blocks(&blocks, styles);
        }
        counts
    }

    /// Remove every hidden run, tab and break (see [`Stories::count`]) from
    /// the stories. Only a header or footer part, or a note, that held one is
    /// rewritten, from its modeled blocks: a header or footer keeps its root
    /// start tag ([`crate::hf::rewrite_part`]), a note its own start and end
    /// tags, and the other notes (the separators included) stay byte for
    /// byte. A rewritten part keeps its encoding (UTF-16, a BOM). Hidden runs
    /// in raw XML stay, as in the body. Returns how many were removed.
    pub fn remove_hidden_text(&self, pkg: &mut Package, styles: Option<&StyleSheet>) -> usize {
        let mut removed = 0;
        for (part, kind) in &self.parts {
            match *kind {
                StoryKind::HeaderFooter(is_header) => {
                    let Some(mut blocks) = pkg.header_footer_blocks(part) else {
                        continue;
                    };
                    let n = remove_blocks(&mut blocks, hidden(styles));
                    if n > 0 {
                        let inner = crate::serialize::blocks_to_xml(&blocks);
                        crate::hf::rewrite_part(pkg, part, is_header, &inner);
                        removed += n;
                    }
                }
                StoryKind::Notes(element) => {
                    removed += remove_hidden_in_notes(pkg, part, element, styles);
                }
            }
        }
        removed
    }

    /// The `w:id` of every comment marker in the stories.
    pub fn comment_marker_ids(&self, pkg: &Package) -> std::collections::BTreeSet<String> {
        let mut ids = std::collections::BTreeSet::new();
        for (part, _) in &self.parts {
            if let Some(xml) = pkg.part_text(part) {
                marker_ids_in_xml(&xml, &mut ids);
            }
        }
        ids
    }

    /// Remove every comment marker from the stories, straight from each
    /// part's XML, not through the model: nothing else in the part changes
    /// but the run a `w:commentReference` leaves holding only its `w:rPr`,
    /// which goes too. A part without a marker is not touched. Returns how
    /// many markers were removed.
    pub fn remove_comment_markers(&self, pkg: &mut Package) -> usize {
        let mut removed = 0;
        for (part, _) in &self.parts {
            let Some(xml) = pkg.part_text(part) else {
                continue;
            };
            let (out, n) = strip_markers_in_part(&xml);
            if n > 0 {
                pkg.set_part_text(part, &out);
                removed += n;
            }
        }
        removed
    }
}

/// [`Stories::remove_hidden_text`] of notes part `part`, whose notes are
/// `element`s: each note that held a hidden run is spliced back alone.
fn remove_hidden_in_notes(
    pkg: &mut Package,
    part: &str,
    element: &str,
    styles: Option<&StyleSheet>,
) -> usize {
    let Some(xml) = pkg.part_text(part) else {
        return 0;
    };
    let rels = pkg.part_rels(part);
    let close = format!("</{element}>");
    let mut out = xml.clone();
    let mut removed = 0;
    // Last first, so the earlier ranges stay valid.
    for range in note_ranges(&xml, element).into_iter().rev() {
        let note = &xml[range.clone()];
        let mut blocks = crate::load::parse_note_blocks(note, &rels);
        let n = remove_blocks(&mut blocks, hidden(styles));
        let Some(head) = tag_end(note).filter(|_| n > 0) else {
            continue;
        };
        let inner = range.start + head..range.end - close.len();
        out.replace_range(inner, &crate::serialize::blocks_to_xml(&blocks));
        removed += n;
    }
    if out != xml {
        pkg.set_part_text(part, &out);
    }
    removed
}

/// The byte ranges of the notes a user wrote in a notes part, each a whole
/// `element` (`w:footnote` / `w:endnote`): not the separator, continuation
/// separator or continuation notice Word keeps there, and not an empty one.
fn note_ranges(xml: &str, element: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some((start, end, filled)) = find_element_from(xml, element, from) {
        from = end;
        let Some(head) = tag_end(&xml[start..]) else {
            break;
        };
        let mut p = XmlParser::new(&xml[start..start + head]);
        let reserved = p.next() == Event::Start
            && matches!(
                p.attr("w:type"),
                "separator" | "continuationSeparator" | "continuationNotice"
            );
        if filled && !reserved {
            out.push(start..end);
        }
    }
    out
}

/// [`Stories::count`] of one story's blocks.
pub fn count_story_blocks(blocks: &[Block], styles: Option<&StyleSheet>) -> StoryCounts {
    StoryCounts {
        hidden: count_blocks(blocks, hidden(styles)),
        hidden_unremovable: count_blocks(blocks, Target::RawHidden),
        comment_markers: count_blocks(blocks, Target::CommentMarkers(None)),
    }
}

/// `xml` (a whole part) without its comment markers, and how many there
/// were. A run a marker leaves holding only its `w:rPr` goes too.
fn strip_markers_in_part(xml: &str) -> (String, usize) {
    const RUN_END: &str = "</w:r>";
    let mut out = xml.to_string();
    let mut removed = 0;
    for name in MARKER_TAGS {
        while let Some((start, end, _)) = find_element_from(&out, name, 0) {
            out.replace_range(start..end, "");
            removed += 1;
            if !out[start..].starts_with(RUN_END) {
                continue;
            }
            // The run around it: the last `<w:r>` / `<w:r …>` before it.
            let run_start = out[..start]
                .match_indices("<w:r")
                .map(|(at, _)| at)
                .filter(|&at| out[at + 4..].starts_with([' ', '>', '\t', '\r', '\n']))
                .last();
            if let Some(run_start) = run_start {
                let run_end = start + RUN_END.len();
                if is_leftover(&out[run_start..run_end]) {
                    out.replace_range(run_start..run_end, "");
                }
            }
        }
    }
    (out, removed)
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
            (
                "word/_rels/document.xml.rels".to_string(),
                HF_RELS.as_bytes().to_vec(),
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

    /// #1215: a comment range can end between two cells of a row (Word's
    /// TableComments files), or start between two rows.
    #[test]
    fn row_level_and_between_row_comment_markers_are_found_and_removed() {
        let cell = |t: &str| format!("<w:tc><w:p><w:r><w:t>{t}</w:t></w:r></w:p></w:tc>");
        let body = format!(
            "<w:tbl><w:commentRangeStart w:id=\"3\"/>\
             <w:tr>{}<w:commentRangeEnd w:id=\"0\"/><w:commentRangeEnd w:id=\"2\"/>{}</w:tr>\
             <w:tr>{}<w:commentRangeEnd w:id=\"3\"/></w:tr></w:tbl>",
            cell("a"),
            cell("b"),
            cell("c"),
        );
        let mut doc = parse(&body);
        let ids: Vec<String> = comment_marker_ids(&doc).into_iter().collect();
        assert_eq!(ids, ["0", "2", "3"]);
        assert!(has_comment_markers(&doc));

        assert_eq!(remove_comment_markers(&mut doc, "0"), 1);
        let ids: Vec<String> = comment_marker_ids(&doc).into_iter().collect();
        assert_eq!(ids, ["2", "3"]);
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("w:id=\"0\""), "{xml}");
        assert!(xml.contains("<w:commentRangeEnd w:id=\"2\"/>"), "{xml}");

        assert_eq!(remove_comment_markers(&mut doc, "3"), 2);
        assert_eq!(remove_all_comment_markers(&mut doc), 1);
        assert!(!has_comment_markers(&doc));
        let Block::Table(t) = &doc.body[0] else {
            panic!("a table");
        };
        // Emptied entries go; the cells stay.
        assert!(t.row_boundaries.is_empty());
        assert!(t.rows.iter().all(|r| r.raw_end.is_empty()));
        assert!(t.rows[0].cells[1].raw_before.is_empty());
        assert_eq!(t.rows[0].cells.len(), 2);
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("comment"), "{xml}");
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
        assert_eq!(count_hidden_runs(&doc, None), 5);
        assert_eq!(remove_hidden_text(&mut doc, None), 5);
        assert_eq!(count_hidden_runs(&doc, None), 0);
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
        assert_eq!(remove_hidden_text(&mut doc, None), 0);
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
        assert_eq!(count_hidden_runs(&doc, None), 0);
        assert_eq!(remove_hidden_text(&mut doc, None), 0);
        assert_eq!(doc, loaded);
    }

    #[test]
    fn inspect_counts_vanish_plus_web_hidden_as_hidden_916() {
        // Hidden text that is also webHidden is still hidden text.
        let body = "<w:p><w:r><w:t>keep</w:t></w:r>\
            <w:r><w:rPr><w:vanish/><w:webHidden/></w:rPr><w:t>gone</w:t></w:r></w:p>";
        let mut doc = parse(body);
        assert_eq!(count_hidden_runs(&doc, None), 1);
        assert_eq!(remove_hidden_text(&mut doc, None), 1);
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
        assert_eq!(count_hidden_runs(&doc, None), 1);
        assert_eq!(remove_hidden_text(&mut doc, None), 1);
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
        assert_eq!(count_hidden_runs(&doc, None), 1);
        assert_eq!(remove_hidden_text(&mut doc, None), 1);
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
        assert_eq!(count_hidden_runs(&doc, None), 1);
        assert_eq!(remove_hidden_text(&mut doc, None), 1);
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("gone"), "{xml}");
        assert!(!xml.contains("comment"), "{xml}");
        assert_eq!(xml.matches("keep").count(), 2, "{xml}");

        // Hidden text, then comments: the second pass counts what it removes.
        let mut doc = parse(&rsid_box());
        assert_eq!(remove_hidden_text(&mut doc, None), 1);
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
        assert_eq!(remove_hidden_text(&mut reopened, None), 1);
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
        assert_eq!(remove_hidden_text(&mut reopened, None), 2);
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

    /// Review r5: the loader declares on the modeled copy's property elements
    /// a prefix the drawing run declares for both copies; the copies are still
    /// twins after a save and reopen, and a removal reaches both.
    #[test]
    fn a_prefix_declared_on_the_drawing_run_keeps_the_copies_twins() {
        let content = "<w:p><w:r><w:rPr><w:lang w:val=\"en-US\" ux:flag=\"1\"/></w:rPr>\
            <w:t xml:space=\"preserve\">keep</w:t></w:r><w:r><w:rPr><w:vanish/>\
            <w:lang w:val=\"en-US\" ux:flag=\"1\"/></w:rPr><w:t xml:space=\"preserve\">gone</w:t></w:r></w:p>";
        let boxed = format!(
            "<w:p><w:r xmlns:ux=\"urn:ux\"><mc:AlternateContent><mc:Choice Requires=\"wps\"><w:drawing><wps:txbx>\
             <w:txbxContent>{content}</w:txbxContent></wps:txbx></w:drawing></mc:Choice>\
             <mc:Fallback><w:pict><v:shape><v:textbox><w:txbxContent>{content}</w:txbxContent>\
             </v:textbox></v:shape></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>"
        );
        let xml = document_to_xml(&parse(&boxed));
        assert!(xml.contains("<w:lang xmlns:ux=\"urn:ux\""), "{xml}");
        let body = &xml[xml.find("<w:body>").unwrap() + 8..xml.find("</w:body>").unwrap()];
        let mut reopened = parse(body);
        assert_eq!(remove_hidden_text(&mut reopened, None), 1);
        let xml = document_to_xml(&reopened);
        assert!(!xml.contains("gone"), "{xml}");
        assert_eq!(xml.matches("keep").count(), 2, "{xml}");
    }

    /// SmartArt and equation runs are kept whole as their raw and written
    /// back verbatim, so a comment reference sharing one is found and
    /// removed there, and the drawing stays.
    #[test]
    fn a_comment_reference_in_a_drawing_run_is_found_and_removed_1107() {
        let reference = "<w:commentReference w:id=\"2\"/>";
        let mut doc = Document {
            body: vec![Block::Paragraph(crate::model::Paragraph {
                content: vec![
                    Inline::SmartArt {
                        raw: format!("<w:r><w:drawing>smart</w:drawing>{reference}</w:r>"),
                        text: Vec::new(),
                    },
                    Inline::Equation {
                        raw: format!("<w:r><w:object>eq</w:object>{reference}</w:r>"),
                        text: "x".into(),
                        latex: None,
                    },
                ],
                ..Default::default()
            })],
        };
        let ids: Vec<String> = comment_marker_ids(&doc).into_iter().collect();
        assert_eq!(ids, ["2"]);
        assert_eq!(count_blocks(&doc.body, Target::CommentMarkers(None)), 2);
        assert_eq!(remove_comment_markers(&mut doc, "2"), 2);
        let xml = document_to_xml(&doc);
        assert_eq!(count_markers(&xml, None), 0, "{xml}");
        assert!(xml.contains("<w:drawing>smart</w:drawing>"), "{xml}");
        assert!(xml.contains("<w:object>eq</w:object>"), "{xml}");
    }

    /// A footnote reference's run is kept whole as its raw, so a comment
    /// reference sharing that run must be found and removed there too.
    #[test]
    fn a_comment_reference_in_a_footnote_run_is_found_and_removed_1107() {
        let body = "<w:p><w:r><w:t>text</w:t></w:r>\
            <w:r><w:footnoteReference w:id=\"1\"/><w:commentReference w:id=\"2\"/></w:r></w:p>";
        let mut doc = parse(body);
        let Block::Paragraph(p) = &doc.body[0] else {
            panic!("expected paragraph");
        };
        assert!(matches!(p.content[1], Inline::FootnoteRef { .. }));
        let ids: Vec<String> = comment_marker_ids(&doc).into_iter().collect();
        assert_eq!(ids, ["2"]);
        assert_eq!(count_blocks(&doc.body, Target::CommentMarkers(None)), 1);
        assert_eq!(remove_comment_markers(&mut doc, "2"), 1);
        let xml = document_to_xml(&doc);
        assert_eq!(count_markers(&xml, None), 0, "{xml}");
        assert!(xml.contains("<w:footnoteReference w:id=\"1\"/>"), "{xml}");
        assert!(comment_marker_ids(&doc).is_empty());
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
        assert_eq!(count_hidden_runs(&doc, None), 0, "none of it is modeled");
        // One in the move, one in the field, B and its fallback twin B'.
        assert_eq!(count_unremovable_hidden_runs(&doc), 4);
        assert_eq!(remove_hidden_text(&mut doc, None), 0);
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

    /// A header holding a comment range and reference, a hidden run and
    /// visible text.
    fn marked_header() -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <w:hdr {W}><w:p><w:commentRangeStart w:id=\"3\"/><w:r><w:t>Head</w:t></w:r>\
             <w:commentRangeEnd w:id=\"3\"/><w:r><w:rPr><w:rStyle w:val=\"CommentReference\"/></w:rPr>\
             <w:commentReference w:id=\"3\"/></w:r>\
             <w:r><w:rPr><w:vanish/></w:rPr><w:t>Hush</w:t></w:r></w:p></w:hdr>"
        )
    }

    /// A notes part `root`/`element` with the two separators and one note
    /// (id 1) holding its reference mark, visible text, a hidden run and a
    /// comment reference.
    fn notes_part(root: &str, element: &str, mark: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <w:{root} {W}>\
             <w:{element} w:type=\"separator\" w:id=\"-1\"><w:p><w:r><w:separator/></w:r></w:p></w:{element}>\
             <w:{element} w:type=\"continuationSeparator\" w:id=\"0\"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:{element}>\
             <w:{element} w:id=\"1\"><w:p><w:r><w:rPr><w:rStyle w:val=\"Mark\"/></w:rPr><w:{mark}/></w:r>\
             <w:r><w:t xml:space=\"preserve\"> Note</w:t></w:r>\
             <w:r><w:rPr><w:vanish/></w:rPr><w:t>Quiet</w:t></w:r>\
             <w:commentRangeStart w:id=\"4\"/><w:commentRangeEnd w:id=\"4\"/>\
             <w:r><w:commentReference w:id=\"4\"/></w:r></w:p></w:{element}>\
             </w:{root}>"
        )
    }

    /// The relationships of the header and footer parts the tests use.
    const HF_RELS: &str = "<?xml version=\"1.0\"?><Relationships>        <Relationship Id=\"rIdH\" Type=\"h\" Target=\"header1.xml\"/>        <Relationship Id=\"rIdH2\" Type=\"h\" Target=\"header2.xml\"/>        <Relationship Id=\"rIdF\" Type=\"f\" Target=\"footer1.xml\"/>        <Relationship Id=\"rIdH9\" Type=\"h\" Target=\"header9.xml\"/></Relationships>";

    /// A trailing sectPr referencing header1 (default), header2 (first page,
    /// not shown: no titlePg) and footer1. header9 is related, but no section
    /// references it.
    const SECT: &str = "<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rIdH\"/>        <w:headerReference w:type=\"first\" r:id=\"rIdH2\"/>        <w:footerReference w:type=\"default\" r:id=\"rIdF\"/></w:sectPr>";

    fn stories(pkg: &Package) -> Stories {
        Stories::new(pkg, &pkg.document)
    }

    fn storied_docx() -> Package {
        let document = format!(
            "<w:document {W}><w:body><w:p><w:r><w:t>Body</w:t></w:r>\
             <w:r><w:footnoteReference w:id=\"1\"/></w:r><w:r><w:endnoteReference w:id=\"1\"/></w:r></w:p>\
             {SECT}</w:body></w:document>"
        );
        let footer = format!("<w:ftr {W}><w:p><w:r><w:t>Foot</w:t></w:r></w:p></w:ftr>");
        docx(
            &document,
            &[
                ("word/header1.xml", &marked_header()),
                ("word/footer1.xml", &footer),
                (
                    "word/footnotes.xml",
                    &notes_part("footnotes", "footnote", "footnoteRef"),
                ),
                (
                    "word/endnotes.xml",
                    &notes_part("endnotes", "endnote", "endnoteRef"),
                ),
            ],
        )
    }

    /// #918: Inspect Document looks in headers, footers and notes too:
    /// the header's hidden run and its comment's three markers are found.
    #[test]
    fn header_hidden_run_and_comment_markers_are_counted_and_removed() {
        let mut pkg = storied_docx();
        assert_eq!(
            stories(&pkg).part_names(),
            [
                "word/header1.xml",
                "word/footer1.xml",
                "word/footnotes.xml",
                "word/endnotes.xml"
            ]
        );
        let counts = stories(&pkg).count(&pkg, None, None);
        assert_eq!(
            counts,
            StoryCounts {
                hidden: 3,
                hidden_unremovable: 0,
                comment_markers: 9,
            }
        );
        let ids: Vec<String> = stories(&pkg).comment_marker_ids(&pkg).into_iter().collect();
        assert_eq!(ids, ["3", "4"]);
        // Leaving the header out (open in an editor) leaves its counts out.
        let rest = stories(&pkg).count(&pkg, None, Some("word/header1.xml"));
        assert_eq!((rest.hidden, rest.comment_markers), (2, 6));

        assert_eq!(stories(&pkg).remove_comment_markers(&mut pkg), 9);
        let header = pkg.part_text("word/header1.xml").unwrap();
        assert_eq!(count_markers(&header, None), 0, "{header}");
        // The reference's run went with it; the rest is as it was.
        assert!(!header.contains("CommentReference"), "{header}");
        assert!(
            header.contains("<w:r><w:t>Head</w:t></w:r><w:r><w:rPr><w:vanish/>"),
            "{header}"
        );
        assert!(stories(&pkg).comment_marker_ids(&pkg).is_empty());

        assert_eq!(stories(&pkg).remove_hidden_text(&mut pkg, None), 3);
        let header = pkg.part_text("word/header1.xml").unwrap();
        assert!(!header.contains("w:vanish"), "{header}");
        assert!(header.contains("Head"), "{header}");
        assert!(header.starts_with("<?xml"), "{header}");
        let blocks = pkg.header_footer_blocks("word/header1.xml").unwrap();
        let text: String = blocks.iter().map(|b| b.plain_text()).collect();
        assert_eq!(text, "Head");
        assert_eq!(
            stories(&pkg).count(&pkg, None, None),
            StoryCounts::default()
        );
    }

    /// #918: a footnote's and an endnote's hidden run and comment markers go;
    /// the separators stay byte for byte, the note keeps its id and its
    /// reference mark, and the body's references are untouched.
    #[test]
    fn footnote_and_endnote_hidden_runs_are_removed_separators_untouched() {
        let mut pkg = storied_docx();
        let body_before = pkg.part_text("word/document.xml").unwrap();
        let separators = |pkg: &Package, part: &str, element: &str| -> Vec<String> {
            let xml = pkg.part_text(part).unwrap();
            let mut out = Vec::new();
            let mut from = 0;
            while let Some((a, b, _)) = find_element_from(&xml, element, from) {
                if xml[a..b].contains("w:type=") {
                    out.push(xml[a..b].to_string());
                }
                from = b;
            }
            out
        };
        let notes = [
            ("word/footnotes.xml", "w:footnote", "w:footnoteRef"),
            ("word/endnotes.xml", "w:endnote", "w:endnoteRef"),
        ];
        let before: Vec<Vec<String>> = notes
            .iter()
            .map(|(part, element, _)| separators(&pkg, part, element))
            .collect();
        assert_eq!(stories(&pkg).remove_hidden_text(&mut pkg, None), 3);
        assert_eq!(stories(&pkg).remove_comment_markers(&mut pkg), 9);
        for ((part, element, mark), before) in notes.iter().zip(before) {
            let xml = pkg.part_text(part).unwrap();
            assert_eq!(before.len(), 2, "{xml}");
            assert_eq!(separators(&pkg, part, element), before, "{xml}");
            assert!(!xml.contains("w:vanish"), "{xml}");
            assert!(!xml.contains("Quiet"), "{xml}");
            assert_eq!(count_markers(&xml, None), 0, "{xml}");
            assert!(xml.contains(&format!("<{element} w:id=\"1\">")), "{xml}");
            assert!(xml.contains(&format!("<{mark}/>")), "{xml}");
            assert!(xml.contains(" Note"), "{xml}");
        }
        assert_eq!(pkg.part_text("word/document.xml").unwrap(), body_before);
        let notes = crate::notes::parse_notes(&pkg);
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().all(|n| n.id == 1 && n.text.contains("Note")));
    }

    /// #918: a Remove All that finds nothing in a story leaves its part's
    /// bytes alone: no re-serialization of what did not change.
    #[test]
    fn unchanged_story_parts_are_not_rewritten() {
        let footer = format!("<w:ftr {W}>\n  <w:p><w:r><w:t>Foot</w:t></w:r></w:p>\n</w:ftr>");
        let footnotes = format!(
            "<w:footnotes {W}><w:footnote w:type=\"separator\" w:id=\"-1\"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>\
             <w:footnote w:id=\"1\"><w:p><w:r><w:footnoteRef/></w:r><w:r><w:t>Plain</w:t></w:r></w:p></w:footnote></w:footnotes>"
        );
        let mut pkg = docx(
            &format!(
                "<w:document {W}><w:body><w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>x</w:t></w:r></w:p>{SECT}</w:body></w:document>"
            ),
            &[
                ("word/header1.xml", &marked_header()),
                ("word/footer1.xml", &footer),
                ("word/footnotes.xml", &footnotes),
            ],
        );
        let bytes = |pkg: &Package| -> Vec<Vec<u8>> {
            ["word/footer1.xml", "word/footnotes.xml"]
                .iter()
                .map(|p| pkg.part(p).unwrap().to_vec())
                .collect()
        };
        let before = bytes(&pkg);
        assert_eq!(stories(&pkg).remove_hidden_text(&mut pkg, None), 1);
        assert_eq!(stories(&pkg).remove_comment_markers(&mut pkg), 3);
        assert_eq!(bytes(&pkg), before);
        // And with nothing left anywhere, the header is not rewritten either.
        let header = pkg.part("word/header1.xml").unwrap().to_vec();
        assert_eq!(stories(&pkg).remove_hidden_text(&mut pkg, None), 0);
        assert_eq!(stories(&pkg).remove_comment_markers(&mut pkg), 0);
        assert_eq!(pkg.part("word/header1.xml").unwrap(), header);
    }

    /// #918: with the document's styles, a run hidden by its character
    /// style, its paragraph style (through `basedOn`) or the default
    /// paragraph style counts and goes; a direct `w:vanish w:val="0"` keeps
    /// a run over a hidden style. Without styles only direct `w:vanish` does.
    #[test]
    fn style_inherited_hidden_run_is_counted_and_removed() {
        let styles = crate::styles::parse_styles_xml(
            r#"<w:styles>
            <w:style w:type="character" w:styleId="Secret"><w:rPr><w:vanish/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="Hush"><w:rPr><w:vanish/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="HushChild"><w:basedOn w:val="Hush"/></w:style>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal"/>
        </w:styles>"#,
        );
        let body = "<w:p><w:r><w:t>keep</w:t></w:r>\
            <w:r><w:rPr><w:rStyle w:val=\"Secret\"/></w:rPr><w:t>char</w:t></w:r>\
            <w:hyperlink w:anchor=\"x\"><w:r><w:rPr><w:rStyle w:val=\"Secret\"/></w:rPr><w:t>link</w:t></w:r></w:hyperlink></w:p>\
            <w:p><w:pPr><w:pStyle w:val=\"HushChild\"/></w:pPr><w:r><w:t>para</w:t></w:r>\
            <w:r><w:rPr><w:vanish w:val=\"0\"/></w:rPr><w:t>shown</w:t></w:r></w:p>";
        let doc = parse(body);
        assert_eq!(count_hidden_runs(&doc, None), 0);
        assert_eq!(count_hidden_runs(&doc, Some(&styles)), 3);
        let mut doc = doc;
        assert_eq!(remove_hidden_text(&mut doc, Some(&styles)), 3);
        let text: String = doc.body.iter().map(|b| b.plain_text()).collect();
        assert_eq!(text, "keepshown");
        assert_eq!(count_hidden_runs(&doc, Some(&styles)), 0);

        // The default paragraph style hides a paragraph that names none, and
        // a text box's paragraph is judged by its own style.
        let normal = crate::styles::parse_styles_xml(
            r#"<w:styles><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr><w:vanish/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="Shown"/></w:styles>"#,
        );
        let doc = parse(
            "<w:p><w:r><w:t>a</w:t></w:r></w:p>\
             <w:p><w:pPr><w:pStyle w:val=\"Shown\"/></w:pPr><w:r><w:t>b</w:t></w:r></w:p>",
        );
        assert_eq!(count_hidden_runs(&doc, Some(&normal)), 1);

        // A text box anchored in a paragraph of the hidden default style:
        // its "Shown" paragraph is visible, its unstyled one hidden, and the
        // removal reaches both copies of the box.
        let boxed = two_copy_text_box(
            "<w:p><w:pPr><w:pStyle w:val=\"Shown\"/></w:pPr><w:r><w:t>boxed</w:t></w:r></w:p>\
             <w:p><w:r><w:t>hush</w:t></w:r></w:p>",
        );
        let mut doc = parse(&boxed);
        assert_eq!(count_hidden_runs(&doc, Some(&normal)), 1);
        assert_eq!(remove_hidden_text(&mut doc, Some(&normal)), 1);
        let xml = document_to_xml(&doc);
        assert_eq!(xml.matches("boxed").count(), 2, "{xml}");
        assert!(!xml.contains("hush"), "{xml}");
        assert_eq!(count_hidden_runs(&doc, Some(&normal)), 0);
    }

    /// #918: a direct `w:vanish w:val="0"` over a hidden character style is
    /// not hidden text, in the body or a header.
    #[test]
    fn direct_vanish_off_over_hidden_style_is_not_hidden() {
        let styles = crate::styles::parse_styles_xml(
            r#"<w:styles><w:style w:type="character" w:styleId="Secret"><w:rPr><w:vanish/></w:rPr></w:style></w:styles>"#,
        );
        let run = "<w:r><w:rPr><w:rStyle w:val=\"Secret\"/><w:vanish w:val=\"0\"/></w:rPr><w:t>on</w:t></w:r>";
        let doc = parse(&format!("<w:p>{run}</w:p>"));
        assert_eq!(count_hidden_runs(&doc, Some(&styles)), 0);
        let header = format!("<w:hdr {W}><w:p>{run}</w:p></w:hdr>");
        let mut pkg = docx(
            &format!("<w:document {W}><w:body>{SECT}</w:body></w:document>"),
            &[("word/header2.xml", &header)],
        );
        assert_eq!(stories(&pkg).count(&pkg, Some(&styles), None).hidden, 0);
        assert_eq!(stories(&pkg).remove_hidden_text(&mut pkg, Some(&styles)), 0);
        assert_eq!(pkg.part_text("word/header2.xml").unwrap(), header);
    }

    /// A hidden run in a header's field stays raw: counted as unremovable,
    /// as in the body.
    #[test]
    fn header_hidden_run_in_raw_xml_is_counted_unremovable() {
        let header = format!(
            "<w:hdr {W}><w:p><w:fldSimple w:instr=\" PAGE \"><w:r><w:rPr><w:vanish/></w:rPr><w:t>1</w:t></w:r></w:fldSimple></w:p></w:hdr>"
        );
        let mut pkg = docx(
            &format!("<w:document {W}><w:body>{SECT}</w:body></w:document>"),
            &[("word/header1.xml", &header)],
        );
        let counts = stories(&pkg).count(&pkg, None, None);
        assert_eq!((counts.hidden, counts.hidden_unremovable), (0, 1));
        assert_eq!(stories(&pkg).remove_hidden_text(&mut pkg, None), 0);
        assert_eq!(pkg.part_text("word/header1.xml").unwrap(), header);
    }

    /// #918 FIX r1 M1: a UTF-16 header keeps its encoding, its BOM and its
    /// root start tag (namespaces, `mc:Ignorable`) when its hidden run goes.
    #[test]
    fn utf16_header_keeps_encoding_and_root_when_hidden_text_goes() {
        let root = format!(
            "<w:hdr {W} xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" \
             xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\" mc:Ignorable=\"w14\">"
        );
        let header = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-16\" standalone=\"yes\"?>\n{root}\
             <w:p w14:paraId=\"1A2B3C4D\"><w:r><w:t>Head</w:t></w:r>\
             <w:r><w:rPr><w:vanish/></w:rPr><w:t>Hush</w:t></w:r></w:p></w:hdr>"
        );
        let utf16: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(header.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let document = format!("<w:document {W}><w:body>{SECT}</w:body></w:document>");
        let parts = vec![
            (
                "[Content_Types].xml".to_string(),
                br#"<?xml version="1.0"?><Types/>"#.to_vec(),
            ),
            (
                "_rels/.rels".to_string(),
                br#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#.to_vec(),
            ),
            ("word/document.xml".to_string(), document.into_bytes()),
            ("word/header1.xml".to_string(), utf16),
            (
                "word/_rels/document.xml.rels".to_string(),
                HF_RELS.as_bytes().to_vec(),
            ),
        ];
        let mut pkg = load_package(&write_zip(&parts)).unwrap();
        assert_eq!(stories(&pkg).count(&pkg, None, None).hidden, 1);
        assert_eq!(stories(&pkg).remove_hidden_text(&mut pkg, None), 1);
        let bytes = pkg.part("word/header1.xml").unwrap();
        assert!(
            bytes.starts_with(&[0xff, 0xfe, b'<', 0]),
            "still UTF-16 LE with its BOM"
        );
        let xml = pkg.part_text("word/header1.xml").unwrap();
        assert!(
            xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\""),
            "{xml}"
        );
        // Kept, with only the namespaces the editor's content may use added.
        let kept = root.trim_end_matches('>');
        assert!(xml.contains(kept), "root start tag kept: {xml}");
        assert!(!xml.contains("w:vanish") && !xml.contains("Hush"), "{xml}");
        assert!(xml.contains("Head"), "{xml}");
    }

    /// #918 FIX r1 m1: a header part no section references (kept in the
    /// package for undo, dropped by the save) is not inspected or edited.
    #[test]
    fn unreferenced_header_is_not_a_story() {
        let hidden = format!(
            "<w:hdr {W}><w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>Gone</w:t></w:r>\
             <w:commentRangeStart w:id=\"7\"/></w:p></w:hdr>"
        );
        let mut pkg = docx(
            &format!("<w:document {W}><w:body>{SECT}</w:body></w:document>"),
            &[("word/header9.xml", &hidden)],
        );
        assert!(stories(&pkg).part_names().is_empty());
        assert_eq!(
            stories(&pkg).count(&pkg, None, None),
            StoryCounts::default()
        );
        assert!(stories(&pkg).comment_marker_ids(&pkg).is_empty());
        assert_eq!(stories(&pkg).remove_hidden_text(&mut pkg, None), 0);
        assert_eq!(stories(&pkg).remove_comment_markers(&mut pkg), 0);
        assert_eq!(pkg.part_text("word/header9.xml").unwrap(), hidden);
        // Referenced from a section break, it is one.
        let sect = SECT.replace("rIdH2", "rIdH9");
        let pkg = docx(
            &format!(
                "<w:document {W}><w:body><w:p><w:pPr>{sect}</w:pPr></w:p>{SECT}</w:body></w:document>"
            ),
            &[("word/header9.xml", &hidden)],
        );
        assert_eq!(stories(&pkg).part_names(), ["word/header9.xml"]);
        assert_eq!(stories(&pkg).count(&pkg, None, None).hidden, 1);
    }
}
