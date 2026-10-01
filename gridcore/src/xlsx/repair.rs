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
    OoxmlNs, STRICT, SheetPackage, TRANSITIONAL, XlsxError, find_element_by_attr,
    is_strict_workbook, load_parts, minimal_styles_xml, open_container, override_element,
    parse_rels, rels_part_name, resolve_relative, workbook_part_name,
};
use crate::sheet::Workbook;

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
    let refs = relationships(&parts);
    let mut fixes: Vec<(String, Fix)> = Vec::new();
    for name in damaged.iter().filter(|n| !n.ends_with(".rels")) {
        let types: Vec<&str> = refs
            .iter()
            .filter(|r| r.target == *name)
            .map(|r| r.ty.as_str())
            .collect();
        match classify(&types) {
            Some(fix) => fixes.push((name.clone(), fix)),
            None => return Err(XlsxError::Unrepairable(name.clone())),
        }
    }
    // A damaged rels part can only go with its own part: an emptied
    // worksheet's or a dropped part's. Any other leaves `r:id`s unresolved.
    for name in damaged.iter().filter(|n| n.ends_with(".rels")) {
        let owner_goes = fixes.iter().any(|(part, fix)| {
            rels_part_name(part) == *name && matches!(fix, Fix::Drop | Fix::Stub(Stub::Worksheet))
        });
        if !owner_goes {
            return Err(XlsxError::Unrepairable(name.clone()));
        }
    }

    let mut styles_emptied = false;
    for (name, fix) in fixes {
        match fix {
            Fix::Stub(stub) => {
                if stub == Stub::Worksheet {
                    // Every element its rels served is gone with the sheet's
                    // XML, so they would only name parts nothing uses.
                    let own = rels_part_name(&name);
                    parts.retain(|(n, _)| *n != own);
                }
                styles_emptied |= stub == Stub::Styles;
                let xml = stub_xml(stub, ns, &parts);
                parts.push((name.clone(), xml.into_bytes()));
                repairs.emptied.push(name);
            }
            Fix::Drop => {
                drop_part(&mut parts, &name);
                repairs.dropped.push(name);
            }
        }
    }
    let mut pkg = load_parts(parts)?;
    if styles_emptied {
        reset_cell_styles(&mut pkg.workbook);
    }
    Ok((pkg, repairs))
}

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

/// The minimal valid part for `stub`.
fn stub_xml(stub: Stub, ns: &OoxmlNs, parts: &[(String, Vec<u8>)]) -> String {
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
        Stub::Styles => minimal_styles_xml(ns.sml, dxf_count_needed(parts)),
        Stub::Drawing => format!(
            "{DECL}\n<xdr:wsDr xmlns:xdr=\"{}\" xmlns:a=\"{}\"/>",
            ns.xdr, ns.dml
        ),
    }
}

/// One more than the highest differential format any part names
/// (`dxfId`, `headerRowDxfId`, `dataDxfId`, …), so each still resolves to an
/// (empty) `<dxf/>` once the styles are emptied.
fn dxf_count_needed(parts: &[(String, Vec<u8>)]) -> usize {
    let mut need = 0;
    for (_, bytes) in parts.iter().filter(|(n, _)| n.ends_with(".xml")) {
        let xml = String::from_utf8_lossy(bytes);
        let mut rest: &str = &xml;
        while let Some(i) = rest.find("xfId=\"") {
            let before = rest.as_bytes()[..i].last().copied();
            let after = &rest[i + "xfId=\"".len()..];
            if matches!(before, Some(b'd' | b'D')) {
                let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
                if let Ok(id) = digits.parse::<usize>() {
                    need = need.max(id + 1);
                }
            }
            rest = after;
        }
    }
    need
}

/// Leave `name` out of `parts`, with every relationship that targets it, its
/// content-type override and its own rels part.
fn drop_part(parts: &mut Vec<(String, Vec<u8>)>, name: &str) {
    let own_rels = rels_part_name(name);
    parts.retain(|(n, _)| n != name && *n != own_rels);
    for (rels_name, bytes) in parts.iter_mut().filter(|(n, _)| n.ends_with(".rels")) {
        let dir = rels_source_dir(rels_name).to_string();
        let mut xml = String::from_utf8_lossy(bytes).into_owned();
        let mut changed = false;
        while let Some(el) = find_element_by_attr(&xml, "Relationship", "Target", |t| {
            resolve_relative(&dir, t) == name
        }) {
            xml.replace_range(el.start..el.end, "");
            changed = true;
        }
        if changed {
            *bytes = xml.into_bytes();
        }
    }
    if let Some((_, bytes)) = parts.iter_mut().find(|(n, _)| n == "[Content_Types].xml") {
        let mut xml = String::from_utf8_lossy(bytes).into_owned();
        while let Some(el) = override_element(&xml, &format!("/{name}")) {
            xml.replace_range(el.start..el.end, "");
        }
        *bytes = xml.into_bytes();
    }
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
