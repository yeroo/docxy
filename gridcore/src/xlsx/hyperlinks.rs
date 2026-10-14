//! Cell hyperlinks: one reader of a `<hyperlink>` element for the loader and
//! the save, and the save's writer for links the model has and the file
//! doesn't (#1025): an AutoCorrect URL typed in-session, a retyped target.
//! A sheet whose links match what its file loads as keeps its worksheet and
//! rels parts byte for byte.

use std::collections::{BTreeMap, HashMap, HashSet};

use opccore::xml::{Event, XmlParser};

use super::{
    OoxmlNs, append_all_to_worksheet_child, decode, element_children, esc_attr, parse_rels,
    parse_rels_mode, put_worksheet_child, rels_part_name, rels_takes, remove_worksheet_child,
    start_tag, with_decl, worksheet_child_span, worksheet_root, worksheet_takes,
};
use crate::sheet::{Sheet, cell_name, parse_cell_name, parse_range_name};

/// A cell rectangle: (r1, c1, r2, c2), 0-based and inclusive.
type Rect = (u32, u32, u32, u32);

/// A range link over more cells than this applies only to its anchor (a
/// whole-column link would otherwise spread over a million cells).
const SPREAD_LIMIT: u64 = 4096;

/// External hyperlink targets keyed by relationship id, from a worksheet's
/// parsed rels.
pub(super) fn hyperlink_targets(rels: &[(String, String, String)]) -> HashMap<String, String> {
    rels.iter()
        .filter(|(_, ty, _)| ty.ends_with("/hyperlink"))
        .map(|(id, _, t)| (id.clone(), t.clone()))
        .collect()
}

/// One `<hyperlink>`'s rectangle and target, from its raw `ref`, `r:id` and
/// `location` attribute values: an external URL (through `targets`), else
/// `#` and the decoded in-workbook location. `None` for an element the
/// loader skips: no readable `ref`, or no target.
pub(super) fn hyperlink_of(
    ref_attr: &str,
    rid: &str,
    location: &str,
    targets: &HashMap<String, String>,
) -> Option<(Rect, String)> {
    let rect = parse_range_name(ref_attr)
        .or_else(|| parse_cell_name(ref_attr).map(|(r, c)| (r, c, r, c)))?;
    let target = if !rid.is_empty() {
        targets.get(rid).cloned()
    } else if !location.is_empty() {
        Some(format!("#{}", decode(location)))
    } else {
        None
    }?;
    Some((rect, target))
}

/// The cells a link over `rect` sets: all of them, or only the anchor past
/// [`SPREAD_LIMIT`].
pub(super) fn link_cells(rect: Rect) -> impl Iterator<Item = (u32, u32)> {
    let (r1, c1, r2, c2) = rect;
    let cells = (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64;
    let (r2, c2) = if cells > SPREAD_LIMIT {
        (r1, c1)
    } else {
        (r2, c2)
    };
    (r1..=r2).flat_map(move |r| (c1..=c2).map(move |c| (r, c)))
}

/// A `<hyperlink>` element of the block: its span in the block, and what
/// [`hyperlink_of`] reads from it.
struct Item {
    span: (usize, usize),
    link: Option<(Rect, String)>,
}

/// The `<hyperlink>` children of a `<hyperlinks>` block, read as the loader
/// reads them.
fn items(block: &str, targets: &HashMap<String, String>) -> Vec<Item> {
    element_children(block)
        .into_iter()
        .filter(|(name, _, _)| super::local(name) == "hyperlink")
        .map(|(_, a, b)| {
            let link = start_tag(&block[a..b]).and_then(|tag| {
                let attr = |name: &str| {
                    tag.attrs
                        .iter()
                        .find(|(n, _, _)| *n == name)
                        .map_or("", |&(_, vs, ve)| &block[a + vs..a + ve])
                };
                hyperlink_of(attr("ref"), attr("r:id"), attr("location"), targets)
            });
            Item { span: (a, b), link }
        })
        .collect()
}

/// What the loader makes of `items`, later elements winning.
fn replay<'a>(items: impl Iterator<Item = &'a Item>) -> HashMap<(u32, u32), &'a str> {
    let mut out = HashMap::new();
    for item in items {
        if let Some((rect, target)) = &item.link {
            for cell in link_cells(*rect) {
                out.insert(cell, target.as_str());
            }
        }
    }
    out
}

/// Bring the worksheet's `<hyperlinks>` in line with `sheet.hyperlinks`:
/// a loaded element whose cells the model changed goes (its relationship
/// stays, unreferenced, which OPC allows), and each model cell the
/// remaining elements don't give its target gets a `<hyperlink>` of its
/// own, an external target through a `TargetMode="External"` relationship
/// in the sheet's rels part (created when missing). A sheet whose links
/// load as the model holds them is returned unchanged, and its rels part
/// is not touched. A link that can't be written (the worksheet has no
/// place for the block, the rels part can't take a relationship) is left
/// out rather than written with an rId that names nothing.
pub(super) fn write_hyperlinks(
    xml: String,
    sheet: &Sheet,
    parts: &mut Vec<(String, Vec<u8>)>,
    part_name: &str,
    ns: &OoxmlNs,
) -> String {
    let span = if xml.contains("hyperlink") {
        worksheet_child_span(&xml, "hyperlinks")
    } else {
        None
    };
    if sheet.hyperlinks.is_empty() && span.is_none() {
        return xml;
    }
    let rels_part = rels_part_name(part_name);
    let targets = parts
        .iter()
        .find(|(n, _)| *n == rels_part)
        .map(|(_, b)| hyperlink_targets(&parse_rels(&String::from_utf8_lossy(b))))
        .unwrap_or_default();
    let (block_start, block) = match span {
        Some((s, e)) => (s, &xml[s..e]),
        None => (0, ""),
    };
    let items = items(block, &targets);

    // The cells whose link the model changed since the load.
    let loaded = replay(items.iter());
    let mut changed: HashSet<(u32, u32)> = sheet
        .hyperlinks
        .iter()
        .filter(|(cell, t)| loaded.get(cell) != Some(&t.as_str()))
        .map(|(&cell, _)| cell)
        .collect();
    changed.extend(
        loaded
            .keys()
            .filter(|cell| !sheet.hyperlinks.contains_key(cell))
            .copied(),
    );
    if changed.is_empty() {
        return xml;
    }

    // An element the loader read and the model changed goes; one it can't
    // read is never touched.
    let stale: Vec<bool> = items
        .iter()
        .map(|item| {
            item.link
                .as_ref()
                .is_some_and(|(rect, _)| link_cells(*rect).any(|c| changed.contains(&c)))
        })
        .collect();
    let kept = replay(
        items
            .iter()
            .zip(&stale)
            .filter(|(_, s)| !**s)
            .map(|(i, _)| i),
    );
    let new: BTreeMap<(u32, u32), &str> = sheet
        .hyperlinks
        .iter()
        .filter(|(cell, t)| kept.get(cell) != Some(&t.as_str()))
        .map(|(&cell, t)| (cell, t.as_str()))
        .collect();

    // No block and no place for one: nothing can be written.
    if span.is_none() && !worksheet_takes(&xml, "hyperlinks", false) {
        return xml;
    }
    let mut rels = ExternalRels::new(parts, rels_part, ns.rel("hyperlink"));
    let mut added = Vec::new();
    let mut uses_r = false;
    for (&(r, c), target) in &new {
        let at = cell_name(r, c);
        let item = match target.strip_prefix('#') {
            Some(location) => format!(
                "<hyperlink ref=\"{at}\" location=\"{}\"/>",
                esc_attr(location)
            ),
            None => match rels.id(target) {
                Some(rid) => {
                    uses_r = true;
                    format!("<hyperlink ref=\"{at}\" r:id=\"{rid}\"/>")
                }
                None => continue,
            },
        };
        added.push(item);
    }
    rels.finish(parts);

    // Strike the stale elements.
    let struck = match span {
        Some(_) if added.is_empty() && !stale.is_empty() && stale.iter().all(|&s| s) => {
            return remove_worksheet_child(&xml, "hyperlinks");
        }
        Some((_, block_end)) if stale.iter().any(|&s| s) => {
            let mut kept_block = String::with_capacity(block.len());
            let mut from = 0;
            for (item, _) in items.iter().zip(&stale).filter(|(_, s)| **s) {
                kept_block.push_str(&block[from..item.span.0]);
                from = item.span.1;
            }
            kept_block.push_str(&block[from..]);
            Some(format!(
                "{}{kept_block}{}",
                &xml[..block_start],
                &xml[block_end..]
            ))
        }
        _ => None,
    };
    let mut out = struck.unwrap_or(xml);
    if added.is_empty() {
        return out;
    }
    if worksheet_child_span(&out, "hyperlinks").is_some() {
        // Into the existing block: `r:` bound on the root when it binds
        // nothing, else on each element that uses it.
        if uses_r {
            if let Some(root) = worksheet_root(&out) {
                match root.r_ns.as_deref() {
                    Some(r) if r == ns.rels => {}
                    None => out.insert_str(root.tag_close, &format!(" xmlns:r=\"{}\"", ns.rels)),
                    Some(_) => {
                        for item in added.iter_mut().filter(|i| i.contains(" r:id=")) {
                            *item = with_decl(item, &format!("xmlns:r=\"{}\"", ns.rels));
                        }
                    }
                }
            }
        }
        append_all_to_worksheet_child(&out, "hyperlinks", &added).unwrap_or(out)
    } else {
        let block = format!("<hyperlinks>{}</hyperlinks>", added.concat());
        let rels = uses_r.then_some(ns.rels);
        put_worksheet_child(&out, "hyperlinks", &block, rels, false)
    }
}

/// A worksheet's rels part taking External hyperlink relationships, read
/// once and written back once ([`ExternalRels::finish`]), so a save adding
/// many links stays linear.
struct ExternalRels {
    rels_part: String,
    rel_type: String,
    /// The part as it is, or as it will be created; `None` when it can't
    /// take a relationship ([`rels_takes`]), or would be created in a
    /// package whose content types give `.rels` parts no type.
    xml: Option<String>,
    exists: bool,
    /// The External relationships of `rel_type` there or added: target → id.
    have: HashMap<String, String>,
    next: u32,
    added: String,
}

impl ExternalRels {
    fn new(parts: &[(String, Vec<u8>)], rels_part: String, rel_type: String) -> ExternalRels {
        let current = parts
            .iter()
            .find(|(n, _)| *n == rels_part)
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned());
        let exists = current.is_some();
        let mut have = HashMap::new();
        for (id, ty, target, mode) in current.as_deref().map(parse_rels_mode).unwrap_or_default() {
            if ty == rel_type && mode.as_deref() == Some("External") {
                have.entry(target).or_insert(id);
            }
        }
        let takes = rels_takes(parts, &rels_part, false) && (exists || rels_have_a_type(parts));
        let xml = takes.then(|| current.unwrap_or_else(|| EMPTY_RELS.to_string()));
        let next = xml.as_deref().map_or(0, super::max_rid) + 1;
        ExternalRels {
            rels_part,
            rel_type,
            xml,
            exists,
            have,
            next,
            added: String::new(),
        }
    }

    /// The id of an External relationship to `target`: an equal one already
    /// there, else a new one with the next free `rIdN`.
    fn id(&mut self, target: &str) -> Option<String> {
        if let Some(id) = self.have.get(target) {
            return Some(id.clone());
        }
        self.xml.as_ref()?;
        let rid = format!("rId{}", self.next);
        self.next += 1;
        self.added.push_str(&format!(
            "<Relationship Id=\"{rid}\" Type=\"{}\" Target=\"{}\" TargetMode=\"External\"/>",
            self.rel_type,
            esc_attr(target)
        ));
        self.have.insert(target.to_string(), rid.clone());
        Some(rid)
    }

    /// Write the added relationships into the part, creating it if needed.
    fn finish(self, parts: &mut Vec<(String, Vec<u8>)>) {
        let (Some(xml), false) = (self.xml, self.added.is_empty()) else {
            return;
        };
        let xml = xml.replacen(
            "</Relationships>",
            &format!("{}</Relationships>", self.added),
            1,
        );
        if self.exists {
            if let Some(p) = parts.iter_mut().find(|(n, _)| *n == self.rels_part) {
                p.1 = xml.into_bytes();
            }
        } else {
            parts.push((self.rels_part, xml.into_bytes()));
        }
    }
}

/// A new rels part, as [`super::add_rel`] creates one.
const EMPTY_RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>";

/// Whether `[Content_Types].xml` gives a `.rels` part a type: a `Default`
/// for the `rels` extension (OPC compares extensions case-insensitively).
fn rels_have_a_type(parts: &[(String, Vec<u8>)]) -> bool {
    let Some((_, bytes)) = parts.iter().find(|(n, _)| n == "[Content_Types].xml") else {
        return false;
    };
    let xml = String::from_utf8_lossy(bytes);
    let mut p = XmlParser::new(&xml);
    loop {
        match p.next() {
            Event::Start if super::local(p.name()) == "Default" => {
                if decode(p.attr("Extension")).eq_ignore_ascii_case("rels") {
                    return true;
                }
            }
            Event::Eof => return false,
            _ => {}
        }
    }
}
