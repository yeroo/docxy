//! Excel's *Open and Repair*: load a workbook whose container has entries
//! that cannot be read, by mending or leaving out each of them.
//!
//! A part is referenced by more than its `.rels` entry: the `r:id`, the `s=`
//! or the `dxfId` that uses it sits in a part that is kept and saved mostly
//! verbatim. Pruning only the relationship would leave that reference
//! dangling, so each damaged entry is one of four kinds:
//!
//! - **required** (the content types, the package and workbook rels, the
//!   workbook part): the load fails, as [`load_xlsx`] does;
//! - **emptied**: a worksheet, the shared strings, the styles or a drawing
//!   is replaced by a minimal valid part, so every reference to it still
//!   resolves;
//! - **dropped**: a part nothing names but a relationship (theme, calcChain,
//!   document properties, custom XML, the VBA project) goes, with those
//!   relationships, its content-type override and its own rels;
//! - anything else fails with [`XlsxError::Unrepairable`] naming the part.

use super::{
    OoxmlNs, STRICT, SheetPackage, TRANSITIONAL, XlsxError, decode, is_strict_workbook, load_parts,
    local, minimal_styles_xml, open_container, parse_rels, rels_part_name, resolve_relative,
    workbook_part_name,
};
use crate::sheet::Workbook;
use opccore::xml::{Event, XmlParser};
use std::collections::{HashMap, HashSet};

/// What [`load_xlsx_repair`] did to the damaged entries, by part name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Repairs {
    /// Replaced by a minimal valid part (an empty sheet, no strings, default
    /// styles, no drawings).
    pub emptied: Vec<String>,
    /// Left out, with every relationship and content-type override naming it.
    pub dropped: Vec<String>,
}

impl Repairs {
    /// Nothing needed repairing.
    pub fn is_empty(&self) -> bool {
        self.emptied.is_empty() && self.dropped.is_empty()
    }
}

/// The minimal part a damaged entry is replaced by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stub {
    Worksheet,
    SharedStrings,
    Styles,
    Drawing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fix {
    Stub(Stub),
    Drop,
}

/// [`load_xlsx`], keeping every part that can be read and mending the rest
/// (see the module docs). A workbook with nothing damaged loads exactly as
/// [`load_xlsx`] loads it, with empty [`Repairs`].
pub fn load_xlsx_repair(data: &[u8]) -> Result<(SheetPackage, Repairs), XlsxError> {
    let zip = open_container(data)?;
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    let mut damaged: Vec<String> = Vec::new();
    for e in zip.entries() {
        match zip.extract(e) {
            Some(bytes) => parts.push((e.name.clone(), bytes)),
            None => damaged.push(e.name.clone()),
        }
    }
    let mut repairs = Repairs::default();
    if damaged.is_empty() {
        return load_parts(parts).map(|pkg| (pkg, repairs));
    }
    // A damaged file has a handful of unreadable entries; a container with
    // hundreds is not a workbook worth mending, and each one costs work
    // below (#610 r4).
    if damaged.len() > MAX_DAMAGED {
        return Err(XlsxError::CorruptPart);
    }
    // A name the central directory lists twice, one copy of it damaged, is
    // ambiguous: which copy did the workbook mean? Mending it would also
    // write two parts under one name.
    let readable: HashSet<&str> = parts.iter().map(|(n, _)| n.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    for name in &damaged {
        if !seen.insert(name) || readable.contains(name.as_str()) {
            return Err(XlsxError::Unrepairable(name.clone()));
        }
    }

    if damaged
        .iter()
        .any(|n| n == "[Content_Types].xml" || n == "_rels/.rels")
    {
        return Err(XlsxError::CorruptPart);
    }
    let wb_part = workbook_part_name(&parts);
    if damaged.contains(&wb_part) {
        return Err(XlsxError::MissingWorkbook);
    }
    if damaged.contains(&rels_part_name(&wb_part)) {
        return Err(XlsxError::CorruptPart);
    }
    let strict = parts
        .iter()
        .find(|(n, _)| *n == wb_part)
        .is_some_and(|(_, b)| is_strict_workbook(&String::from_utf8_lossy(b)));
    let ns = if strict { &STRICT } else { &TRANSITIONAL };

    // Classify everything against the relationships as they were read, so the
    // order of the damaged entries cannot change an answer.
    let mut types_of: HashMap<String, Vec<String>> = HashMap::new();
    for rel in relationships(&parts) {
        types_of.entry(rel.target).or_default().push(rel.ty);
    }
    let mut fixes: Vec<(String, Fix)> = Vec::new();
    for name in damaged.iter().filter(|n| !n.ends_with(".rels")) {
        let types: Vec<&str> = types_of
            .get(name)
            .map(|t| t.iter().map(String::as_str).collect())
            .unwrap_or_default();
        match classify(&types) {
            Some(fix) => fixes.push((name.clone(), fix)),
            None => return Err(XlsxError::Unrepairable(name.clone())),
        }
    }
    // A damaged rels part can only go with its own part: an emptied
    // worksheet's or a dropped part's. Any other leaves `r:id`s unresolved.
    let owners_go: HashSet<String> = fixes
        .iter()
        .filter(|(_, fix)| matches!(fix, Fix::Drop | Fix::Stub(Stub::Worksheet)))
        .map(|(part, _)| rels_part_name(part))
        .collect();
    for name in damaged.iter().filter(|n| n.ends_with(".rels")) {
        if !owners_go.contains(name) {
            return Err(XlsxError::Unrepairable(name.clone()));
        }
    }

    // Everything that goes, in one pass each over the package: the dropped
    // parts, their rels and an emptied worksheet's rels (every element they
    // served went with the sheet's XML) leave it, and every relationship and
    // override naming a dropped part leaves its rels part and the content
    // types.
    let dropped: HashSet<&str> = fixes
        .iter()
        .filter(|(_, fix)| *fix == Fix::Drop)
        .map(|(name, _)| name.as_str())
        .collect();
    let leaving: HashSet<String> = owners_go
        .into_iter()
        .chain(dropped.iter().map(|n| n.to_string()))
        .collect();
    parts.retain(|(n, _)| !leaving.contains(n));
    if !dropped.is_empty() {
        prune_references(&mut parts, &dropped);
    }
    let styles_emptied = fixes.iter().any(|(_, fix)| *fix == Fix::Stub(Stub::Styles));
    // The emptied styles part is made once, after the runaway references are
    // gone, however many parts a styles relationship names.
    let styles_xml = styles_emptied.then(|| {
        strip_runaway_dxf_ids(&mut parts);
        minimal_styles_xml(ns.sml, dxf_count_needed(&parts))
    });
    for (name, fix) in fixes {
        match fix {
            Fix::Stub(stub) => {
                let xml = match (stub, &styles_xml) {
                    (Stub::Styles, Some(xml)) => xml.clone(),
                    _ => stub_xml(stub, ns),
                };
                parts.push((name.clone(), xml.into_bytes()));
                repairs.emptied.push(name);
            }
            Fix::Drop => repairs.dropped.push(name),
        }
    }
    let mut pkg = load_parts(parts)?;
    if styles_emptied {
        reset_cell_styles(&mut pkg.workbook);
    }
    Ok((pkg, repairs))
}

/// The most unreadable entries [`load_xlsx_repair`] mends. A damaged
/// workbook has a handful; past this the container is refused outright, so
/// a crafted one cannot make the mending take time without bound.
const MAX_DAMAGED: usize = 256;

/// One relationship, its type lowercased and its target resolved to a part
/// name.
struct Rel {
    ty: String,
    target: String,
}

/// The directory a rels part's targets resolve against:
/// `xl/worksheets/_rels/sheet1.xml.rels` → `xl/worksheets`.
fn rels_source_dir(rels_part: &str) -> &str {
    match rels_part.rsplit_once("/_rels/") {
        Some((dir, _)) => dir,
        None => "",
    }
}

fn relationships(parts: &[(String, Vec<u8>)]) -> Vec<Rel> {
    let mut out = Vec::new();
    for (name, bytes) in parts.iter().filter(|(n, _)| n.ends_with(".rels")) {
        let dir = rels_source_dir(name);
        for (_, ty, target) in parse_rels(&String::from_utf8_lossy(bytes)) {
            out.push(Rel {
                ty: ty.to_ascii_lowercase(),
                target: resolve_relative(dir, &target),
            });
        }
    }
    out
}

/// How a damaged part named by relationships of `types` is mended, or `None`
/// when it cannot be without breaking what references it.
fn classify(types: &[&str]) -> Option<Fix> {
    // Nothing names it, so nothing breaks without it.
    if types.is_empty() {
        return Some(Fix::Drop);
    }
    let fix = |ty: &str| -> Option<Fix> {
        let kind = ty.rsplit('/').next().unwrap_or(ty);
        Some(match kind {
            "worksheet" => Fix::Stub(Stub::Worksheet),
            "sharedstrings" => Fix::Stub(Stub::SharedStrings),
            "styles" => Fix::Stub(Stub::Styles),
            "drawing" => Fix::Stub(Stub::Drawing),
            "theme"
            | "calcchain"
            | "vbaproject"
            | "core-properties"
            | "extended-properties"
            | "custom-properties"
            | "customxml"
            | "customxmlprops"
            | "thumbnail" => Fix::Drop,
            _ => return None,
        })
    };
    let first = fix(types[0])?;
    // Two relationships asking different things of one part: no safe answer.
    types
        .iter()
        .all(|ty| fix(ty) == Some(first))
        .then_some(first)
}

/// The minimal valid part for `stub`. An emptied styles part is made by the
/// caller, once, padded to [`dxf_count_needed`].
fn stub_xml(stub: Stub, ns: &OoxmlNs) -> String {
    const DECL: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
    match stub {
        Stub::Worksheet => format!(
            "{DECL}\n<worksheet xmlns=\"{}\"><sheetData/></worksheet>",
            ns.sml
        ),
        Stub::SharedStrings => format!(
            "{DECL}\n<sst xmlns=\"{}\" count=\"0\" uniqueCount=\"0\"></sst>",
            ns.sml
        ),
        Stub::Styles => minimal_styles_xml(ns.sml, 0),
        Stub::Drawing => format!(
            "{DECL}\n<xdr:wsDr xmlns:xdr=\"{}\" xmlns:a=\"{}\"/>",
            ns.xdr, ns.dml
        ),
    }
}

/// The most empty differential formats an emptied styles part is given. A
/// real workbook names a few hundred at most; a `dxfId` past this is crafted
/// or corrupt, and padding up to it would take memory without bound (#610).
const MAX_STUB_DXFS: u64 = 65_536;

/// Every differential-format reference in `xml` (`dxfId`, `headerRowDxfId`,
/// `dataDxfId`, …) with a numeric value: the attribute's span, from the space
/// before its name to after its closing quote, and the id. An id too long
/// for a `u64` reads as `u64::MAX`. Only an attribute counts: the match must
/// sit inside a start tag (the nearest `<` or `>` before it is a `<`), so
/// text that spells one out, a shared string say, is left alone.
fn dxf_refs(xml: &str) -> Vec<(usize, usize, u64)> {
    const NEEDLE: &str = "xfId=\"";
    let bytes = xml.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    // The nearest '<' or '>' before `scanned`, kept between matches so each
    // byte is looked at once however many matches a part holds (#610 r3).
    let mut bracket: Option<u8> = None;
    let mut scanned = 0;
    while let Some(i) = xml[from..].find(NEEDLE) {
        let at = from + i;
        let value = at + NEEDLE.len();
        from = value;
        // The cheap rejects first: not a `…dxfId`, or not a number.
        if !matches!(at.checked_sub(1).map(|p| bytes[p]), Some(b'd' | b'D')) {
            continue;
        }
        let digits = bytes[value..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits == 0 || bytes.get(value + digits) != Some(&b'"') {
            continue;
        }
        if let Some(p) = bytes[scanned..at]
            .iter()
            .rposition(|b| matches!(b, b'<' | b'>'))
        {
            bracket = Some(bytes[scanned + p]);
        }
        scanned = at;
        if bracket != Some(b'<') {
            continue;
        }
        let id = xml[value..value + digits]
            .parse::<u64>()
            .unwrap_or(u64::MAX);
        let mut start = at;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b':') {
            start -= 1;
        }
        if start > 0 && bytes[start - 1].is_ascii_whitespace() {
            start -= 1;
        }
        out.push((start, value + digits + 1, id));
    }
    out
}

/// One more than the highest differential format any part names, so each
/// still resolves to an (empty) `<dxf/>` once the styles are emptied. Run
/// after [`strip_runaway_dxf_ids`], it is at most [`MAX_STUB_DXFS`].
fn dxf_count_needed(parts: &[(String, Vec<u8>)]) -> usize {
    let need = parts
        .iter()
        .filter(|(n, _)| n.ends_with(".xml"))
        .flat_map(|(_, bytes)| dxf_refs(&String::from_utf8_lossy(bytes)))
        .map(|(_, _, id)| id.saturating_add(1))
        .max()
        .unwrap_or(0);
    need.min(MAX_STUB_DXFS) as usize
}

/// Remove every differential-format reference at or past [`MAX_STUB_DXFS`],
/// as the cell styles are reset: the format it named is gone with the styles
/// either way, and an emptied styles part cannot hold that many. Only a part
/// that had one is rewritten.
fn strip_runaway_dxf_ids(parts: &mut [(String, Vec<u8>)]) {
    for (_, bytes) in parts.iter_mut().filter(|(n, _)| n.ends_with(".xml")) {
        let xml = String::from_utf8_lossy(bytes).into_owned();
        let runaway: Vec<_> = dxf_refs(&xml)
            .into_iter()
            .filter(|&(_, _, id)| id >= MAX_STUB_DXFS)
            .collect();
        if runaway.is_empty() {
            continue;
        }
        // One pass: copy what lies between the removed attributes.
        let mut kept = String::with_capacity(xml.len());
        let mut copied = 0;
        for (start, end, _) in runaway {
            kept.push_str(&xml[copied..start]);
            copied = end;
        }
        kept.push_str(&xml[copied..]);
        *bytes = kept.into_bytes();
    }
}

/// Remove every relationship whose target is in `dropped` from each rels
/// part, and every content-type override naming one, each part in one
/// forward pass ([`without_elements`]). Only a part that named one is
/// rewritten.
fn prune_references(parts: &mut [(String, Vec<u8>)], dropped: &HashSet<&str>) {
    let overrides: HashSet<String> = dropped
        .iter()
        .map(|n| format!("/{n}").to_ascii_lowercase())
        .collect();
    for (name, bytes) in parts.iter_mut() {
        let pruned = if name.ends_with(".rels") {
            let dir = rels_source_dir(name);
            without_elements(
                &String::from_utf8_lossy(bytes),
                "Relationship",
                "Target",
                |t| dropped.contains(resolve_relative(dir, t).as_str()),
            )
        } else if name == "[Content_Types].xml" {
            without_elements(
                &String::from_utf8_lossy(bytes),
                "Override",
                "PartName",
                |v| overrides.contains(&v.to_ascii_lowercase()),
            )
        } else {
            None
        };
        if let Some(xml) = pruned {
            *bytes = xml.into_bytes();
        }
    }
}

/// `xml` without every `<name ...>` element, content and all, whose `attr`
/// satisfies `want`: found in one forward pass and copied around in
/// another. `None` when none matched. A truncated element ends the search,
/// as it does for `find_element_by_attr`.
fn without_elements(
    xml: &str,
    name: &str,
    attr: &str,
    want: impl Fn(&str) -> bool,
) -> Option<String> {
    let mut p = XmlParser::new(xml);
    let mut spans = Vec::new();
    loop {
        match p.next() {
            Event::Start if local(p.name()) == name => {
                let hit = p
                    .attrs()
                    .iter()
                    .any(|a| local(a.name) == attr && want(&decode(a.value)));
                if hit {
                    let start = p.start_pos();
                    if !p.skip_element_complete() {
                        break;
                    }
                    spans.push((start, p.pos()));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if spans.is_empty() {
        return None;
    }
    let mut kept = String::with_capacity(xml.len());
    let mut copied = 0;
    for (start, end) in spans {
        kept.push_str(&xml[copied..start]);
        copied = end;
    }
    kept.push_str(&xml[copied..]);
    Some(kept)
}

/// After the styles were emptied only `cellXfs` 0 exists, so every cell, row
/// and column style goes back to it.
fn reset_cell_styles(wb: &mut Workbook) {
    for sheet in &mut wb.sheets {
        for cell in sheet.cells.values_mut() {
            cell.style = 0;
        }
        for col in &mut sheet.col_defs {
            col.attrs = without_attrs(&col.attrs, &["style"]);
        }
        for attrs in sheet.row_attrs.values_mut() {
            *attrs = without_attrs(attrs, &["s", "customFormat"]);
        }
    }
}

/// A raw attribute string as the loader keeps it, each one led by a space
/// (` ht="15" s="3" customFormat="1"`), without the attributes in `drop`.
fn without_attrs(attrs: &str, drop: &[&str]) -> String {
    let mut out = String::new();
    let mut rest = attrs.trim_start();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else {
            out.push(' ');
            out.push_str(rest);
            break;
        };
        let name = rest[..eq].trim();
        let value_start = rest[eq + 1..].find(['"', '\'']).map(|i| eq + 1 + i);
        let Some(q) = value_start else {
            out.push(' ');
            out.push_str(rest);
            break;
        };
        let quote = rest.as_bytes()[q] as char;
        let end = match rest[q + 1..].find(quote) {
            Some(i) => q + 1 + i + 1,
            None => rest.len(),
        };
        if !drop.contains(&name) {
            out.push(' ');
            out.push_str(rest[..end].trim());
        }
        rest = rest[end..].trim_start();
    }
    out
}

#[cfg(test)]
mod tests;
