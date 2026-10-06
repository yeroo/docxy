//! `.xlsx` bytes ⇄ [`Workbook`], preserving everything we don't model.
//!
//! The same round-trip strategy that keeps `.docx` files safe in `docxcore`:
//! keep **every** original ZIP part, and on save rewrite only what we edited —
//! the `<sheetData>` (and `<cols>`/`<dimension>`) of each worksheet is
//! regenerated and **spliced into the original worksheet XML**, so sheet-level
//! features we don't model (conditional formatting, data validation, drawings,
//! sheet views, merges…) ride along untouched.
//!
//! Additional save rules:
//! - New text goes into `sharedStrings.xml` by appending; existing entries are
//!   never rewritten, so rich-text strings survive.
//! - `xl/calcChain.xml` is dropped (with its content-type override and
//!   relationship) and `<calcPr>` gets `fullCalcOnLoad="1"` — Excel rebuilds
//!   the chain and recalculates, so a stale chain can never corrupt anything.
//! - Shared formulas are expanded to per-cell formulas at load (via reference
//!   translation); groups whose master doesn't parse are preserved verbatim.

use std::collections::{BTreeMap, HashMap};

use opccore::xml::{Event, XmlParser};
use opccore::zip::ZipArchive;
use opccore::zipwrite::write_zip;

mod consolidate;
#[cfg(test)]
mod filter_tests;
mod page;
mod repair;
pub use repair::{Repairs, load_xlsx_repair};

use crate::formula::{file_formula, translate_formula};
use crate::sheet::{
    Cell, CellMeta, CellValue, ColDef, DefinedName, NumFmt, Sheet, Styles, Table, Workbook, Xf,
    cell_name, classify_builtin, classify_format_code, is_array_f, parse_cell_name,
    parse_range_name, ref_starts_at, with_ref,
};

const OLE2: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XlsxError {
    /// Not a ZIP container at all.
    NotZip,
    /// An OLE2 compound file — the legacy binary `.xls` format.
    LegacyXls,
    CorruptPart,
    MissingWorkbook,
    NotUtf8,
    /// [`load_xlsx_repair`] met a damaged part it can neither empty nor drop
    /// without leaving references to it broken.
    Unrepairable(String),
}

impl std::fmt::Display for XlsxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            XlsxError::NotZip => "not an .xlsx file (not a ZIP container)",
            XlsxError::LegacyXls => {
                "legacy binary .xls files are not supported — save as .xlsx first"
            }
            XlsxError::CorruptPart => "corrupt part in .xlsx container",
            XlsxError::MissingWorkbook => "no xl/workbook.xml in container",
            XlsxError::NotUtf8 => "workbook XML is not valid UTF-8",
            XlsxError::Unrepairable(part) => {
                return write!(f, "could not repair: {part} is damaged");
            }
        })
    }
}

impl std::error::Error for XlsxError {}

/// A data-validation rule for [`SheetPackage::add_data_validations`].
#[derive(Clone, Copy, Debug)]
pub struct NewValidation<'a> {
    pub range: (u32, u32, u32, u32),
    pub kind: &'a str,
    pub operator: &'a str,
    pub formula1: &'a str,
    pub formula2: Option<&'a str>,
    /// The rest of the rule (alert, input message, blanks, dropdown) when it
    /// is a copy of one: written as the source has it. `None` is a rule with
    /// the defaults of one typed in (blanks, input message and alert on).
    pub settings: Option<&'a crate::sheet::DataValidation>,
}

/// A loaded `.xlsx`: the editable [`Workbook`] plus all original parts (and
/// the original worksheet XML sources for splicing) so save preserves what
/// isn't modeled.
#[derive(Debug, Clone)]
pub struct SheetPackage {
    pub(crate) parts: Vec<(String, Vec<u8>)>,
    /// Worksheet part name per `workbook.sheets` index.
    pub(crate) sheet_parts: Vec<String>,
    /// Shared strings as loaded (plain text per `<si>`).
    shared: Vec<String>,
    /// Resolved part name of the workbook's shared-strings table, if it has one
    /// (it need not be the conventional `xl/sharedStrings.xml`). Save appends to
    /// this exact part instead of assuming the standard path — otherwise an
    /// unconventional original would be left orphaned beside a fresh duplicate.
    shared_part: Option<String>,
    /// The workbook is Strict Open XML: parts and relationships the writer
    /// creates use the Strict namespaces, as its own parts do.
    strict: bool,
    /// The editable workbook. Mutate it, then [`save_xlsx`].
    pub workbook: Workbook,
}

impl SheetPackage {
    /// Names of all parts in the container (for inspection/tests).
    pub fn part_names(&self) -> Vec<&str> {
        self.parts.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Insert or replace a custom part (e.g. the gridcore model part).
    /// It rides along with save like any preserved part.
    pub fn set_part(&mut self, name: &str, bytes: Vec<u8>) {
        match self.parts.iter_mut().find(|(n, _)| n == name) {
            Some(p) => p.1 = bytes,
            None => self.parts.push((name.to_string(), bytes)),
        }
    }

    /// Remove a part by name (no-op when absent).
    pub fn remove_part(&mut self, name: &str) {
        self.parts.retain(|(n, _)| n != name);
    }

    /// Excel's *Always create backup* (`<workbookPr backupFile>`,
    /// Save As › Tools › General Options): true when the attribute is "1"
    /// or "true" (corpus/xlsx spells the boolean "false"/"true"). Anything
    /// else — another value, no attribute, no element — means off. The flag
    /// is read from the package because it only matters at save time.
    pub fn always_create_backup(&self) -> bool {
        let name = workbook_part_name(&self.parts);
        let Some(bytes) = self.part(&name) else {
            return false;
        };
        let xml = String::from_utf8_lossy(bytes);
        let mut p = XmlParser::new(&xml);
        loop {
            match p.next() {
                Event::Start if local(p.name()) == "workbookPr" => {
                    return matches!(p.attr("backupFile"), "1" | "true");
                }
                Event::Eof => return false,
                _ => {}
            }
        }
    }

    /// The raw bytes of a part by name.
    pub fn part(&self, name: &str) -> Option<&[u8]> {
        self.parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
    }

    /// Does `sheet`'s worksheet part take a new `<tag>` ([`worksheet_takes`])?
    /// A sheet with no part takes anything: the edit is the model's alone, as
    /// it always was.
    pub(crate) fn sheet_takes(&self, sheet: usize, tag: &str, join: bool) -> bool {
        match self.sheet_parts.get(sheet).and_then(|n| self.part(n)) {
            Some(b) => worksheet_takes(&String::from_utf8_lossy(b), tag, join),
            None => true,
        }
    }

    /// The namespaces for anything the writer creates in this package.
    pub(crate) fn ns(&self) -> &'static OoxmlNs {
        if self.strict { &STRICT } else { &TRANSITIONAL }
    }
}

/// The namespace URIs the writer mints, per conformance class. Strict Open
/// XML (ECMA-376 Part 1, Strict) moves them to `purl.oclc.org/ooxml`; the OPC
/// package namespaces and the content types are the same in both.
pub(crate) struct OoxmlNs {
    pub(crate) sml: &'static str,
    /// The `r:` namespace, which is also the prefix of every relationship type.
    pub(crate) rels: &'static str,
    pub(crate) dml: &'static str,
    pub(crate) chart: &'static str,
    pub(crate) xdr: &'static str,
}

impl OoxmlNs {
    /// The relationship type `kind` (`worksheet`, `sharedStrings`, …).
    pub(crate) fn rel(&self, kind: &str) -> String {
        format!("{}/{kind}", self.rels)
    }
}

pub(crate) const TRANSITIONAL: OoxmlNs = OoxmlNs {
    sml: "http://schemas.openxmlformats.org/spreadsheetml/2006/main",
    rels: "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    dml: "http://schemas.openxmlformats.org/drawingml/2006/main",
    chart: "http://schemas.openxmlformats.org/drawingml/2006/chart",
    xdr: "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing",
};

pub(crate) const STRICT: OoxmlNs = OoxmlNs {
    sml: "http://purl.oclc.org/ooxml/spreadsheetml/main",
    rels: "http://purl.oclc.org/ooxml/officeDocument/relationships",
    dml: "http://purl.oclc.org/ooxml/drawingml/main",
    chart: "http://purl.oclc.org/ooxml/drawingml/chart",
    xdr: "http://purl.oclc.org/ooxml/drawingml/spreadsheetDrawing",
};

/// Is the workbook part's root element in the Strict SpreadsheetML namespace?
fn is_strict_workbook(wb_xml: &str) -> bool {
    let mut p = XmlParser::new(wb_xml);
    loop {
        match p.next() {
            Event::Start => {
                let decl = match p.name().split_once(':') {
                    Some((prefix, _)) => format!("xmlns:{prefix}"),
                    None => "xmlns".to_string(),
                };
                return p
                    .namespace_attrs()
                    .iter()
                    .any(|a| a.name == decl && a.value == STRICT.sml);
            }
            Event::Eof => return false,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Load
// ---------------------------------------------------------------------------

/// Open an `.xlsx` from bytes, keeping all parts for a lossless-ish save.
pub fn load_xlsx(data: &[u8]) -> Result<SheetPackage, XlsxError> {
    let zip = open_container(data)?;
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    for e in zip.entries() {
        let bytes = zip.extract(e).ok_or(XlsxError::CorruptPart)?;
        parts.push((e.name.clone(), bytes));
    }
    load_parts(parts)
}

/// The ZIP container of an `.xlsx`, or why `data` is not one.
fn open_container(data: &[u8]) -> Result<ZipArchive<'_>, XlsxError> {
    match ZipArchive::open(data) {
        Some(z) => Ok(z),
        None if data.len() >= 8 && data[..8] == OLE2 => Err(XlsxError::LegacyXls),
        None => Err(XlsxError::NotZip),
    }
}

/// Build the package from its extracted parts: [`load_xlsx`] after reading
/// the container, and [`load_xlsx_repair`] after mending the parts it could
/// not read.
fn load_parts(parts: Vec<(String, Vec<u8>)>) -> Result<SheetPackage, XlsxError> {
    let get = |name: &str| {
        parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
    };
    let get_str = |name: &str| get(name).map(|b| String::from_utf8_lossy(b).into_owned());

    let wb_part = workbook_part_name(&parts);
    let wb_xml = get_str(&wb_part).ok_or(XlsxError::MissingWorkbook)?;
    let wb_dir = match wb_part.rfind('/') {
        Some(i) => &wb_part[..i],
        None => "",
    };
    let wb_rels_name = format!(
        "{}/_rels/{}.rels",
        wb_dir,
        &wb_part[wb_dir.len() + usize::from(!wb_dir.is_empty())..]
    );
    let rels = get_str(&wb_rels_name)
        .map(|xml| parse_rels(&xml))
        .unwrap_or_default();
    // Absolute, relative, `./` and `../` Targets alike, as every other
    // relationship lookup resolves them.
    let resolve = |target: &str| -> String { resolve_relative(wb_dir, target) };

    // Workbook: sheet list + date system + defined names.
    let (sheet_meta, date1904, iterate, raw_names) = parse_workbook_xml(&wb_xml);
    let active_tab = parse_active_tab(&wb_xml).min(sheet_meta.len().saturating_sub(1));

    // Shared strings + styles (relative to the workbook dir). Keep the resolved
    // shared-strings part name so save can append to it in place.
    let shared_part = rels
        .iter()
        .find(|(_, ty, _)| ty.ends_with("/sharedStrings"))
        .map(|(_, _, t)| resolve(t));
    let shared = match &shared_part {
        Some(p) => get_str(p)
            .map(|xml| parse_shared_strings(&xml))
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let styles = rels
        .iter()
        .find(|(_, ty, _)| ty.ends_with("/styles"))
        .and_then(|(_, _, t)| get_str(&resolve(t)))
        .map(|xml| parse_styles(&xml))
        .unwrap_or_default();

    let mut sheets = Vec::new();
    let mut sheet_parts = Vec::new();
    let mut tables: Vec<Table> = Vec::new();
    let mut pending_pivots: Vec<(usize, String)> = Vec::new();
    // Each sheet's and table's `<autoFilter>`, resolved once styles and
    // values are all loaded.
    let mut auto_filters: Vec<(usize, crate::filter::AutoFilter)> = Vec::new();
    // localSheetId counts workbook.xml order; map it to model indices in
    // case a sheet part is missing and gets skipped.
    let mut orig_to_model: Vec<Option<usize>> = Vec::new();
    for (name, rid, hidden) in sheet_meta {
        let part = rels
            .iter()
            .find(|(id, _, _)| *id == rid)
            .map(|(_, _, t)| resolve(t));
        let Some(part) = part else {
            orig_to_model.push(None);
            continue;
        };
        let Some(xml) = get_str(&part) else {
            orig_to_model.push(None);
            continue;
        };
        // Read the sheet's own rels once — for hyperlink targets and tables/pivots.
        let ws_dir = part.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let ws_file = part.rsplit_once('/').map(|(_, f)| f).unwrap_or(&part);
        let ws_rels_name = format!("{ws_dir}/_rels/{ws_file}.rels");
        let ws_rels = get_str(&ws_rels_name)
            .map(|xml| parse_rels(&xml))
            .unwrap_or_default();
        // External hyperlink URLs keyed by relationship id.
        let hlink_targets: std::collections::HashMap<String, String> = ws_rels
            .iter()
            .filter(|(_, ty, _)| ty.ends_with("/hyperlink"))
            .map(|(id, _, t)| (id.clone(), t.clone()))
            .collect();

        let mut sheet = parse_worksheet(&xml, &shared, &hlink_targets);
        sheet.name = name;
        sheet.hidden = hidden;
        sheet.filter_mode = Some(read_filter_mode(&xml));
        sheet.auto_filter = sheet_auto_filter_span(&xml)
            .and_then(|(s, e)| auto_filter_position(&xml[s..e], &styles.dxfs));
        let sheet_idx = sheets.len();
        if let Some(af) = crate::filter::parse_auto_filter(&xml, &styles.dxfs) {
            auto_filters.push((sheet_idx, af));
        }
        orig_to_model.push(Some(sheet_idx));

        // A worksheet names exactly ONE drawing part, through the `r:id` on its
        // `<drawing/>` element. Its rels can list more (an orphan left behind by
        // another writer); taking whichever came last would show artwork Excel
        // itself doesn't, and hang the anchor rewrite off the wrong part.
        let drawing_rid = attr_of_tag(&xml, "<drawing ", "r:id");
        // Excel Tables and pivot tables attached to this worksheet.
        for (rel_id, ty, target) in &ws_rels {
            if ty.ends_with("/table") {
                let table_part = resolve_relative(ws_dir, target);
                if let Some(txml) = get_str(&table_part) {
                    if let Some(t) = parse_table_xml(&txml, sheet_idx, &table_part) {
                        tables.push(t);
                    }
                    if let Some(af) = crate::filter::parse_auto_filter(&txml, &styles.dxfs) {
                        auto_filters.push((sheet_idx, af));
                    }
                }
            } else if ty.ends_with("/pivotTable") {
                pending_pivots.push((sheet_idx, resolve_relative(ws_dir, target)));
            } else if ty.ends_with("/drawing")
                && drawing_rid.as_deref().is_none_or(|want| want == rel_id)
            {
                // Floating pictures/charts anchored to this worksheet.
                let dpart = resolve_relative(ws_dir, target);
                if let Some(dxml) = get_str(&dpart) {
                    let ddir = dpart.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                    let dfile = dpart.rsplit_once('/').map(|(_, f)| f).unwrap_or(&dpart);
                    let drels = get_str(&format!("{ddir}/_rels/{dfile}.rels"))
                        .map(|x| parse_rels(&x))
                        .unwrap_or_default();
                    let resolve_rid = |rid: &str| -> Option<(String, String)> {
                        drels
                            .iter()
                            .find(|(id, _, _)| id == rid)
                            .map(|(_, ty, t)| (ty.to_ascii_lowercase(), resolve_relative(ddir, t)))
                    };
                    sheet.drawings = crate::drawing::parse_drawings(&dxml, &resolve_rid, &get_str);
                    sheet.drawing_part = Some(dpart.clone());
                }
            }
        }

        sheets.push(sheet);
        sheet_parts.push(part);
    }
    if sheets.is_empty() {
        return Err(XlsxError::MissingWorkbook);
    }
    // Tell filter-hidden rows from hand-hidden ones (both are `hidden="1"`).
    for (i, af) in &auto_filters {
        let rows = crate::filter::filtered_rows(&sheets[*i], &styles, date1904, af);
        sheets[*i].filtered_rows.extend(rows);
    }
    // Rich errors: Excel writes a typed `#SPILL!`/`#CALC!` (and
    // `#GETTING_DATA`) as `<v>#VALUE!</v>` with a `vm` pointing at the real
    // error in the rich-value parts.
    let part_of = |suffix: &str, default: &str| {
        rels.iter()
            .find(|(_, ty, _)| ty.ends_with(suffix))
            .map(|(_, _, t)| resolve(t))
            .unwrap_or_else(|| resolve(default))
    };
    let rich = match get_str(&sheet_metadata_part(&parts).0) {
        Some(meta) => rich_error_codes(
            &meta,
            get_str(&part_of("/rdRichValue", "richData/rdrichvalue.xml")).as_deref(),
            get_str(&part_of(
                "/rdRichValueStructure",
                "richData/rdrichvaluestructure.xml",
            ))
            .as_deref(),
        ),
        None => HashMap::new(),
    };
    if !rich.is_empty() {
        for sheet in &mut sheets {
            for cell in sheet.cells.values_mut() {
                decode_rich_error(cell, &rich);
            }
        }
    }
    let defined_names = raw_names
        .into_iter()
        .map(|(name, scope, formula)| DefinedName {
            name,
            scope: scope.and_then(|i| orig_to_model.get(i).copied().flatten()),
            formula,
        })
        .collect();

    // Pivot tables: wire each pivot part to its cache through workbook.xml's
    // <pivotCaches> (cacheId → r:id → cache part).
    let cache_parts: Vec<(u32, String)> = parse_pivot_cache_ids(&wb_xml)
        .into_iter()
        .filter_map(|(cache_id, rid)| {
            rels.iter()
                .find(|(id, _, _)| *id == rid)
                .map(|(_, _, t)| (cache_id, resolve(t)))
        })
        .collect();
    let mut pivots = Vec::new();
    for (sheet_idx, pivot_part) in pending_pivots {
        let Some(xml) = get_str(&pivot_part) else {
            continue;
        };
        let Some((mut piv, cache_id)) =
            crate::pivot::parse_pivot_table_xml(&xml, sheet_idx, &pivot_part)
        else {
            continue;
        };
        let cache = cache_parts
            .iter()
            .find(|(id, _)| *id == cache_id)
            .and_then(|(_, part)| get_str(part).map(|xml| (part.clone(), xml)));
        match cache.and_then(|(part, xml)| {
            crate::pivot::parse_pivot_cache_xml(&xml, date1904).map(|c| (part, c))
        }) {
            Some((cache_part, (source, fields, field_items, calc_formulas, cache_unsupported))) => {
                piv.cache_part = cache_part;
                piv.source = source;
                piv.fields = fields;
                piv.field_items = field_items;
                piv.calc_formulas = calc_formulas;
                piv.unsupported |= cache_unsupported;
            }
            None => piv.unsupported = true,
        }
        pivots.push(piv);
    }

    let strict = is_strict_workbook(&wb_xml);
    Ok(SheetPackage {
        parts,
        sheet_parts,
        shared,
        shared_part,
        strict,
        workbook: Workbook {
            sheets,
            styles,
            defined_names,
            tables,
            removed_tables: Vec::new(),
            pivots,
            date1904,
            iterate,
            active_tab,
        },
    })
}

/// The workbook part, located via the package rels (virtually always
/// `xl/workbook.xml`, but resolve it properly).
fn workbook_part_name(parts: &[(String, Vec<u8>)]) -> String {
    parts
        .iter()
        .find(|(n, _)| n == "_rels/.rels")
        .and_then(|(_, b)| {
            parse_rels(&String::from_utf8_lossy(b))
                .into_iter()
                .find(|(_, ty, _)| ty.ends_with("/officeDocument"))
                .map(|(_, _, target)| target.trim_start_matches('/').to_string())
        })
        .unwrap_or_else(|| "xl/workbook.xml".to_string())
}

/// The workbook's cell/value metadata part (`xl/metadata.xml`, found through
/// its `sheetMetadata` relationship), and whether that relationship exists.
/// Load and save both go through this, so they agree on the part.
fn sheet_metadata_part(parts: &[(String, Vec<u8>)]) -> (String, bool) {
    let wb_part = workbook_part_name(parts);
    let wb_dir = wb_part.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let rels_name = rels_part_name(&wb_part);
    let target = parts
        .iter()
        .find(|(n, _)| *n == rels_name)
        .and_then(|(_, b)| {
            parse_rels(&String::from_utf8_lossy(b))
                .into_iter()
                .find(|(_, ty, _)| ty.ends_with("/sheetMetadata"))
                .map(|(_, _, t)| t)
        });
    match target {
        Some(t) => (resolve_relative(wb_dir, &t), true),
        None => (resolve_relative(wb_dir, "metadata.xml"), false),
    }
}

/// The rels part that belongs to `part`: `xl/workbook.xml` →
/// `xl/_rels/workbook.xml.rels`.
pub(crate) fn rels_part_name(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

/// `<pivotCaches><pivotCache cacheId="0" r:id="rId4"/></pivotCaches>` in
/// workbook.xml → (cacheId, rId) pairs.
fn parse_pivot_cache_ids(wb_xml: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(wb_xml);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == "pivotCache" => {
                if let Ok(id) = p.attr("cacheId").parse::<u32>() {
                    let rid = p
                        .attrs()
                        .iter()
                        .find(|a| local(a.name) == "id")
                        .map(|a| a.value.to_string())
                        .unwrap_or_default();
                    out.push((id, rid));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Resolve a rels target relative to a directory ("../tables/table1.xml"
/// against "xl/worksheets" → "xl/tables/table1.xml").
pub(crate) fn resolve_relative(dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Parse one xl/tables/*.xml part.
fn parse_table_xml(xml: &str, sheet_idx: usize, part: &str) -> Option<Table> {
    let mut p = XmlParser::new(xml);
    let mut name = String::new();
    let mut range = None;
    let mut header_rows = 1u32;
    let mut totals_rows = 0u32;
    let mut columns = Vec::new();
    let mut ids: Vec<Option<u32>> = Vec::new();
    // Each column's `calculatedColumnFormula`, and whether its text is
    // being read.
    let mut calculated: Vec<Option<String>> = Vec::new();
    let mut in_calc = false;
    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "table" => {
                    name = decode(p.attr("displayName"));
                    if name.is_empty() {
                        name = decode(p.attr("name"));
                    }
                    range = parse_range_name(p.attr("ref"));
                    if let Ok(h) = p.attr("headerRowCount").parse::<u32>() {
                        header_rows = h;
                    }
                    if let Ok(t) = p.attr("totalsRowCount").parse::<u32>() {
                        totals_rows = t;
                    }
                }
                "tableColumn" => {
                    columns.push(decode(p.attr("name")));
                    ids.push(p.attr("id").parse().ok().filter(|&id| id != 0));
                    calculated.push(None);
                }
                "calculatedColumnFormula" => {
                    in_calc = !matches!(p.attr("array"), "1" | "true");
                    if let (true, Some(f)) = (in_calc, calculated.last_mut()) {
                        f.get_or_insert_default();
                    }
                }
                _ => {}
            },
            Event::Text if in_calc => {
                if let Some(Some(f)) = calculated.last_mut() {
                    push_text(&p, f);
                }
            }
            Event::End if local(p.name()) == "calculatedColumnFormula" => in_calc = false,
            Event::Eof => break,
            _ => {}
        }
    }
    // An empty element, or an array formula (`array="1"`, which a new row
    // can't take as a plain formula), gives no formula.
    for f in &mut calculated {
        if f.as_deref().is_some_and(|t| t.trim().is_empty()) {
            *f = None;
        }
    }
    if calculated.iter().all(Option::is_none) {
        calculated.clear();
    }
    // Ids the part doesn't give for every column, or gives twice, can't
    // tell its columns apart: the save then matches them by name.
    let mut column_ids: Vec<u32> = ids.iter().flatten().copied().collect();
    let mut sorted = column_ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    if column_ids.len() != columns.len() || sorted.len() != column_ids.len() {
        column_ids.clear();
    }
    Some(Table {
        name,
        sheet: sheet_idx,
        range: range?,
        header_rows,
        totals_rows,
        columns,
        column_ids,
        calculated_formulas: calculated,
        part: part.to_string(),
    })
}

/// Replace a loaded `#VALUE!` whose `vm` names a rich error with the real
/// error, remembering the body so save writes the file's form back.
fn decode_rich_error(cell: &mut Cell, rich: &HashMap<u32, &'static str>) {
    if cell.value != CellValue::Error("#VALUE!".into()) {
        return;
    }
    let Some(meta) = cell.meta.as_deref_mut() else {
        return;
    };
    let Some((vm, snapshot)) = meta.vm.as_mut() else {
        return;
    };
    let Some(code) = vm.parse::<u32>().ok().and_then(|i| rich.get(&i)) else {
        return;
    };
    if *code == "#VALUE!" {
        return;
    }
    cell.value = CellValue::Error((*code).to_string());
    *snapshot = cell.value.clone();
    meta.vm_body = Some("#VALUE!".into());
}

/// The error each `vm` index (1-based, as cells carry it) stands for, when
/// it resolves to a rich `_error` value: `xl/metadata.xml` valueMetadata `bk`
/// → `rc t` (1-based metadataType, which must be `XLRICHVALUE`) and `rc v`
/// (0-based futureMetadata `bk`) → `xlrd:rvb i` (0-based `rv` in
/// rdrichvalue.xml) → its structure's `errorType` key. Anything that does not
/// resolve is left out.
fn rich_error_codes(
    metadata: &str,
    rich_values: Option<&str>,
    structures: Option<&str>,
) -> HashMap<u32, &'static str> {
    let mut out = HashMap::new();
    let (Some(rich_values), Some(structures)) = (rich_values, structures) else {
        return out;
    };
    // metadata.xml
    let mut types: Vec<String> = Vec::new();
    let mut rich_bks: Vec<Option<usize>> = Vec::new();
    let mut value_bks: Vec<Option<(usize, usize)>> = Vec::new();
    let (mut in_rich_future, mut in_value_meta) = (false, false);
    let mut p = XmlParser::new(metadata);
    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "metadataType" => types.push(decode(p.attr("name"))),
                "futureMetadata" => in_rich_future = p.attr("name") == "XLRICHVALUE",
                "valueMetadata" => in_value_meta = true,
                "bk" if in_rich_future => rich_bks.push(None),
                "bk" if in_value_meta => value_bks.push(None),
                "rvb" if in_rich_future => {
                    if let Some(last) = rich_bks.last_mut() {
                        *last = p.attr("i").parse().ok();
                    }
                }
                "rc" if in_value_meta => {
                    if let (Some(last), Ok(t), Ok(v)) = (
                        value_bks.last_mut(),
                        p.attr("t").parse::<usize>(),
                        p.attr("v").parse::<usize>(),
                    ) {
                        last.get_or_insert((t, v));
                    }
                }
                _ => {}
            },
            Event::End => match local(p.name()) {
                "futureMetadata" => in_rich_future = false,
                "valueMetadata" => in_value_meta = false,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    // rdrichvaluestructure.xml: each structure's type and key names.
    let mut structs: Vec<(String, Vec<String>)> = Vec::new();
    let mut p = XmlParser::new(structures);
    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "s" => structs.push((decode(p.attr("t")), Vec::new())),
                "k" => {
                    if let Some(last) = structs.last_mut() {
                        last.1.push(decode(p.attr("n")));
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    // rdrichvalue.xml: each value's structure and positional values.
    let mut values: Vec<(usize, Vec<String>)> = Vec::new();
    let mut in_v = false;
    let mut p = XmlParser::new(rich_values);
    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "rv" => values.push((p.attr("s").parse().unwrap_or(usize::MAX), Vec::new())),
                "v" => {
                    in_v = true;
                    if let Some(last) = values.last_mut() {
                        last.1.push(String::new());
                    }
                }
                _ => {}
            },
            Event::Text if in_v => {
                if let Some(v) = values.last_mut().and_then(|(_, vs)| vs.last_mut()) {
                    XmlParser::append_decoded(p.text(), v);
                }
            }
            Event::End if local(p.name()) == "v" => in_v = false,
            Event::Eof => break,
            _ => {}
        }
    }
    for (i, bk) in value_bks.iter().enumerate() {
        let Some((t, v)) = *bk else { continue };
        if t == 0 || types.get(t - 1).map(String::as_str) != Some("XLRICHVALUE") {
            continue;
        }
        let Some(Some(rv)) = rich_bks.get(v) else {
            continue;
        };
        let Some((s, vals)) = values.get(*rv) else {
            continue;
        };
        let Some((ty, keys)) = structs.get(*s) else {
            continue;
        };
        if ty != "_error" {
            continue;
        }
        let Some(k) = keys.iter().position(|k| k == "errorType") else {
            continue;
        };
        let code = vals
            .get(k)
            .and_then(|v| v.trim().parse::<u32>().ok())
            .and_then(rich_error_code);
        if let Some(code) = code {
            out.insert(i as u32 + 1, code);
        }
    }
    out
}

/// A rich `_error` value's `errorType` ([MS-XLSX] 2.3.6.1): the classic
/// errors 0-6, then `#GETTING_DATA` 7, `#SPILL!` 8, `#CONNECT!` 9,
/// `#BLOCKED!` 10, `#UNKNOWN!` 11, `#FIELD!` 12, `#CALC!` 13. Only the ones
/// the engine models are decoded.
fn rich_error_code(error_type: u32) -> Option<&'static str> {
    Some(match error_type {
        0 => "#NULL!",
        1 => "#DIV/0!",
        2 => "#VALUE!",
        3 => "#REF!",
        4 => "#NAME?",
        5 => "#NUM!",
        6 => "#N/A",
        7 => "#GETTING_DATA",
        8 => "#SPILL!",
        13 => "#CALC!",
        _ => return None,
    })
}

/// Parse a `.rels` stream into (id, type, target) triples.
pub(crate) fn parse_rels(xml: &str) -> Vec<(String, String, String)> {
    parse_rels_mode(xml)
        .into_iter()
        .map(|(id, ty, target, _)| (id, ty, target))
        .collect()
}

/// A `.rels` stream's relationships with their `TargetMode` (`External`,
/// or `None` for an internal one): (Id, Type, Target, TargetMode), the
/// target as written.
pub(crate) fn parse_rels_mode(xml: &str) -> Vec<(String, String, String, Option<String>)> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == "Relationship" => {
                out.push((
                    decode(p.attr("Id")),
                    decode(p.attr("Type")),
                    decode(p.attr("Target")),
                    Some(decode(p.attr("TargetMode"))).filter(|m| !m.is_empty()),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Sheet (name, r:id) pairs, the 1904 flag, and defined names (name, scope,
/// formula) from `xl/workbook.xml`.
#[allow(clippy::type_complexity)]
fn parse_workbook_xml(
    xml: &str,
) -> (
    Vec<(String, String, bool)>,
    bool,
    Option<(u32, f64)>,
    Vec<(String, Option<usize>, String)>,
) {
    let mut sheets = Vec::new();
    let mut date1904 = false;
    let mut iterate = None;
    let mut names = Vec::new();
    let mut p = XmlParser::new(xml);
    let mut cur_name: Option<(String, Option<usize>, String)> = None;
    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "sheet" => {
                    let name = decode(p.attr("name"));
                    // The relationship attr is r:id under the conventional
                    // prefix; accept any prefix:id.
                    let mut rid = decode(p.attr("r:id"));
                    if rid.is_empty() {
                        for a in p.attrs() {
                            if a.name.ends_with(":id") {
                                rid = decode(a.value);
                                break;
                            }
                        }
                    }
                    let hidden = matches!(p.attr("state"), "hidden" | "veryHidden");
                    sheets.push((name, rid, hidden));
                }
                "workbookPr" => {
                    let v = p.attr("date1904");
                    date1904 = v == "1" || v == "true";
                }
                "calcPr" => {
                    let it = p.attr("iterate");
                    if it == "1" || it == "true" {
                        let count = p.attr("iterateCount").parse().unwrap_or(100);
                        let delta = p.attr("iterateDelta").parse().unwrap_or(0.001);
                        iterate = Some((count, delta));
                    }
                }
                "definedName" => {
                    let scope = p.attr("localSheetId").parse::<usize>().ok();
                    cur_name = Some((decode(p.attr("name")), scope, String::new()));
                }
                _ => {}
            },
            Event::Text => {
                if let Some((_, _, f)) = &mut cur_name {
                    XmlParser::append_decoded(p.text(), f);
                }
            }
            Event::End => {
                if local(p.name()) == "definedName" {
                    if let Some(n) = cur_name.take() {
                        // Excel's built-in names (print area and titles, and
                        // `_FilterDatabase`, which backs the sheet's
                        // `<autoFilter>`) load like any other so they follow
                        // structural edits.
                        if !n.2.is_empty() {
                            names.push(n);
                        }
                    }
                }
            }
            Event::Eof => break,
        }
    }
    (sheets, date1904, iterate, names)
}

/// `<workbookView activeTab>` of the first workbook view; 0 when absent.
fn parse_active_tab(xml: &str) -> usize {
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == "workbookView" => {
                return p.attr("activeTab").parse().unwrap_or(0);
            }
            Event::Eof => return 0,
            _ => {}
        }
    }
}

/// Plain text of each `<si>` (rich-text runs concatenated).
fn parse_shared_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(xml);
    let mut cur: Option<String> = None;
    let mut in_t = false;
    let mut in_rph = false; // phonetic runs are annotations, not content
    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "si" => cur = Some(String::new()),
                "t" if !in_rph => in_t = true,
                "rPh" => in_rph = true,
                _ => {}
            },
            Event::Text => {
                if in_t {
                    if let Some(s) = &mut cur {
                        XmlParser::append_decoded(p.text(), s);
                    }
                }
            }
            Event::End => match local(p.name()) {
                "si" => {
                    if let Some(s) = cur.take() {
                        out.push(s);
                    }
                }
                "t" => in_t = false,
                "rPh" => in_rph = false,
                _ => {}
            },
            Event::Eof => break,
        }
    }
    out
}

/// The display subset of `xl/styles.xml`: cellXfs joined with fonts and
/// number formats.
fn parse_styles(xml: &str) -> Styles {
    #[derive(Default, Clone)]
    struct Font {
        bold: bool,
        italic: bool,
        color: Option<(u8, u8, u8)>,
        color_unresolved: bool,
        size: Option<f64>,
        name: Option<String>,
    }
    let mut numfmts: BTreeMap<u32, NumFmt> = BTreeMap::new();
    let mut codes: BTreeMap<u32, String> = BTreeMap::new();
    let mut fonts: Vec<Font> = Vec::new();
    // Each fill's colour, and whether it is one we can't resolve.
    type Fill = (Option<(u8, u8, u8)>, bool);
    let mut fills: Vec<Fill> = Vec::new();
    let mut xfs: Vec<Xf> = Vec::new();

    let mut dxfs: Vec<crate::sheet::Dxf> = Vec::new();
    let mut cur_dxf: Option<crate::sheet::Dxf> = None;
    let mut dxf_in_font = false;
    let mut dxf_in_fill = false;

    let mut p = XmlParser::new(xml);
    let mut in_fonts = false;
    let mut in_fills = false;
    let mut in_cellxfs = false;
    let mut cur_font: Option<Font> = None;
    let mut cur_fill: Option<Fill> = None;
    // A theme or indexed colour, which we don't resolve. For a font, the
    // default text colour (theme 1, indexed 8 or 64, `auto`) is no colour.
    let unresolved = |p: &XmlParser, font: bool| -> bool {
        if !p.attr("rgb").is_empty() || p.attr("auto") == "1" {
            return false;
        }
        match (p.attr("theme"), p.attr("indexed")) {
            ("", "") => false,
            (t, "") => !(font && t == "1"),
            (_, i) => !(matches!(i, "64") || (font && i == "8")),
        }
    };
    let parse_rgb = |rgb: &str| -> Option<(u8, u8, u8)> {
        if rgb.len() == 8 && rgb.is_ascii() {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&rgb[2..4], 16),
                u8::from_str_radix(&rgb[4..6], 16),
                u8::from_str_radix(&rgb[6..8], 16),
            ) {
                return Some((r, g, b));
            }
        }
        None
    };
    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "numFmt" => {
                    if let Ok(id) = p.attr("numFmtId").parse::<u32>() {
                        let code = decode(p.attr("formatCode"));
                        numfmts.insert(id, classify_format_code(&code));
                        codes.insert(id, code);
                    }
                }
                "fonts" => in_fonts = true,
                "font" if in_fonts => cur_font = Some(Font::default()),
                // A `<dxf>` (differential format) for conditional formatting.
                "dxf" => cur_dxf = Some(crate::sheet::Dxf::default()),
                "font" if cur_dxf.is_some() => dxf_in_font = true,
                "fill" if cur_dxf.is_some() => dxf_in_fill = true,
                "b" => {
                    let on = p.attr("val") != "0" && p.attr("val") != "false";
                    if let Some(f) = &mut cur_font {
                        f.bold = on;
                    } else if dxf_in_font {
                        if let Some(d) = &mut cur_dxf {
                            d.bold = Some(on);
                        }
                    }
                }
                "i" => {
                    let on = p.attr("val") != "0" && p.attr("val") != "false";
                    if let Some(f) = &mut cur_font {
                        f.italic = on;
                    } else if dxf_in_font {
                        if let Some(d) = &mut cur_dxf {
                            d.italic = Some(on);
                        }
                    }
                }
                "color" => {
                    if let Some(f) = &mut cur_font {
                        f.color = parse_rgb(p.attr("rgb"));
                        f.color_unresolved = f.color.is_none() && unresolved(&p, true);
                    } else if dxf_in_font {
                        if let Some(d) = &mut cur_dxf {
                            d.color = parse_rgb(p.attr("rgb"));
                            d.color_unresolved = d.color.is_none() && unresolved(&p, true);
                        }
                    }
                }
                "sz" => {
                    if let Some(f) = &mut cur_font {
                        f.size = p.attr("val").parse().ok();
                    }
                }
                "name" if in_fonts => {
                    if let Some(f) = &mut cur_font {
                        let n = decode(p.attr("val"));
                        if !n.is_empty() {
                            f.name = Some(n);
                        }
                    }
                }
                "fills" => in_fills = true,
                "fill" if in_fills => cur_fill = Some((None, false)),
                // dxf solid fills carry the colour in `<bgColor>` (or `<fgColor>`).
                "bgColor" | "fgColor" => {
                    if let Some(fl) = &mut cur_fill {
                        if p.name().ends_with("fgColor") {
                            fl.0 = parse_rgb(p.attr("rgb"));
                            fl.1 = fl.0.is_none() && unresolved(&p, false);
                        }
                    } else if dxf_in_fill {
                        let rgb = p.attr("rgb");
                        // ARGB alpha "00" = fully transparent → no fill override.
                        let transparent = rgb.len() == 8 && rgb.starts_with("00");
                        if !transparent {
                            if let Some(c) = parse_rgb(rgb) {
                                if let Some(d) = &mut cur_dxf {
                                    d.fill = Some(c);
                                }
                            } else if unresolved(&p, false) {
                                if let Some(d) = &mut cur_dxf {
                                    d.fill_unresolved = d.fill.is_none();
                                }
                            }
                        }
                    }
                }
                "cellXfs" => in_cellxfs = true,
                "xf" if in_cellxfs => {
                    let numfmt_id: u32 = p.attr("numFmtId").parse().unwrap_or(0);
                    let font_id: usize = p.attr("fontId").parse().unwrap_or(0);
                    let fill_id: usize = p.attr("fillId").parse().unwrap_or(0);
                    let numfmt = numfmts
                        .get(&numfmt_id)
                        .copied()
                        .unwrap_or_else(|| classify_builtin(numfmt_id));
                    let code = codes
                        .get(&numfmt_id)
                        .cloned()
                        .or_else(|| crate::numfmt::builtin_code(numfmt_id).map(str::to_string));
                    let font = fonts.get(font_id).cloned().unwrap_or_default();
                    let loaded_from = Some(xfs.len() as u32);
                    xfs.push(Xf {
                        numfmt,
                        code,
                        bold: font.bold,
                        italic: font.italic,
                        color: font.color,
                        fill: fills.get(fill_id).and_then(|f| f.0),
                        fill_unresolved: fills.get(fill_id).is_some_and(|f| f.1),
                        color_unresolved: font.color_unresolved,
                        align: crate::sheet::Align::General,
                        font_size: font.size,
                        font_name: font.name.clone(),
                        border: false,
                        wrap: false,
                        quote_prefix: matches!(p.attr("quotePrefix"), "1" | "true"),
                        loaded_from,
                    });
                }
                "alignment" if in_cellxfs => {
                    if let Some(x) = xfs.last_mut() {
                        x.align = crate::sheet::Align::from_attr(p.attr("horizontal"));
                        x.wrap = matches!(p.attr("wrapText"), "1" | "true");
                    }
                }
                _ => {}
            },
            Event::End => match local(p.name()) {
                "fonts" => in_fonts = false,
                "font" => {
                    dxf_in_font = false;
                    if let Some(f) = cur_font.take() {
                        fonts.push(f);
                    }
                }
                "fills" => in_fills = false,
                "fill" => {
                    dxf_in_fill = false;
                    if let Some(fl) = cur_fill.take() {
                        fills.push(fl);
                    }
                }
                "dxf" => {
                    if let Some(d) = cur_dxf.take() {
                        dxfs.push(d);
                    }
                }
                "cellXfs" => in_cellxfs = false,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    if xfs.is_empty() {
        xfs.push(Xf::default());
    }
    Styles { xfs, dxfs }
}

/// Read the `count="N"` attribute of the element starting at `prefix`.
fn read_count(xml: &str, prefix: &str) -> u32 {
    let Some(s) = xml.find(prefix) else { return 0 };
    let Some(cp) = xml[s..].find("count=\"") else {
        return 0;
    };
    let cs = s + cp + 7;
    let ce = xml[cs..].find('"').map(|x| cs + x).unwrap_or(cs);
    xml[cs..ce].parse().unwrap_or(0)
}

/// Add `delta` to the `count="N"` of the element at `prefix`.
fn bump_count(xml: &str, prefix: &str, delta: u32) -> String {
    if delta == 0 {
        return xml.to_string();
    }
    let Some(s) = xml.find(prefix) else {
        return xml.to_string();
    };
    let Some(cp) = xml[s..].find("count=\"") else {
        return xml.to_string();
    };
    let cs = s + cp + 7;
    let Some(ce) = xml[cs..].find('"').map(|x| cs + x) else {
        return xml.to_string();
    };
    let Ok(n) = xml[cs..ce].parse::<u32>() else {
        return xml.to_string();
    };
    let mut out = xml.to_string();
    out.replace_range(cs..ce, &(n + delta).to_string());
    out
}

/// The raw `<xf>` elements of `<cellXfs>`, in order.
fn cell_xf_elements(xml: &str) -> Vec<&str> {
    let Some(start) = xml.find("<cellXfs") else {
        return Vec::new();
    };
    let end = xml[start..]
        .find("</cellXfs>")
        .map_or(xml.len(), |e| start + e);
    let body = &xml[start..end];
    let mut out = Vec::new();
    let mut i = body.find('>').map_or(body.len(), |e| e + 1);
    while let Some(p) = body[i..].find("<xf") {
        let s = i + p;
        if !body[s + 3..].starts_with([' ', '/', '>', '\t', '\r', '\n']) {
            i = s + 3;
            continue;
        }
        let Some(tag_end) = body[s..].find('>').map(|e| s + e + 1) else {
            break;
        };
        let e = if body[..tag_end].ends_with("/>") {
            tag_end
        } else {
            body[tag_end..]
                .find("</xf>")
                .map_or(body.len(), |x| tag_end + x + 5)
        };
        out.push(&body[s..e]);
        i = e;
    }
    out
}

/// The raw `<font>` elements of `<fonts>`, in order (a `<dxf>`'s `<font>`
/// lives outside `<fonts>` and is not one of them). Read with the loader's
/// parser ([`element_children`]), so comments and prefixes are no trouble.
fn font_elements(xml: &str) -> Vec<&str> {
    let Some(start) = xml
        .find("<fonts")
        .filter(|&s| xml[s + 6..].starts_with([' ', '>', '/', '\t', '\r', '\n']))
    else {
        return Vec::new();
    };
    let fonts = &xml[start..];
    element_children(fonts)
        .into_iter()
        .filter(|(name, _, _)| name == "font")
        .map(|(_, s, e)| &fonts[s..e])
        .collect()
}

/// The child elements of one raw element, as (local name, raw element).
fn child_elements(raw: &str) -> Vec<(String, String)> {
    element_children(raw)
        .into_iter()
        .map(|(name, s, e)| (name, raw[s..e].to_string()))
        .collect()
}

/// A loaded `<font>` with only the children an edit changed rewritten: its
/// underline, strike, theme colour, family, charset and the rest come along.
fn edit_font(raw: &str, from: &Xf, to: &Xf, fmt_size: impl Fn(f64) -> String) -> String {
    // The conventional CT_Font child order, for where a new child goes.
    const ORDER: [&str; 15] = [
        "b",
        "i",
        "strike",
        "condense",
        "extend",
        "outline",
        "shadow",
        "u",
        "vertAlign",
        "sz",
        "color",
        "name",
        "family",
        "charset",
        "scheme",
    ];
    let rank = |n: &str| ORDER.iter().position(|o| *o == n).unwrap_or(ORDER.len());
    let mut kids = child_elements(raw);
    // Drop every `name` child (so never two `<b>`), then add `new` if any.
    let mut set = |name: &str, new: Option<String>| {
        kids.retain(|(n, _)| n != name);
        if let Some(el) = new {
            let at = kids
                .iter()
                .position(|(n, _)| rank(n) > rank(name))
                .unwrap_or(kids.len());
            kids.insert(at, (name.to_string(), el));
        }
    };
    if from.bold != to.bold {
        set("b", to.bold.then(|| "<b/>".to_string()));
    }
    if from.italic != to.italic {
        set("i", to.italic.then(|| "<i/>".to_string()));
    }
    // The model reads only an rgb colour, so a theme or indexed one reads
    // as None: an unchanged colour keeps the source `<color>` (theme, tint
    // and all). Known limit: setting Automatic on a theme-coloured font
    // compares equal and so keeps the theme colour.
    if from.color != to.color {
        set(
            "color",
            to.color
                .map(|(r, g, b)| format!("<color rgb=\"FF{r:02X}{g:02X}{b:02X}\"/>")),
        );
    }
    if from.font_size != to.font_size {
        set(
            "sz",
            Some(format!(
                "<sz val=\"{}\"/>",
                fmt_size(to.font_size.unwrap_or(11.0))
            )),
        );
    }
    if from.font_name != to.font_name {
        set(
            "name",
            Some(format!(
                "<name val=\"{}\"/>",
                esc_attr(to.font_name.as_deref().unwrap_or("Calibri"))
            )),
        );
        // The old font's family and scheme describe it, not the new name; a
        // `<scheme val="minor"/>` would make Excel show the theme font
        // instead of the name just set.
        set("family", None);
        set("scheme", None);
    }
    let mut font = String::from("<font>");
    for (_, el) in &kids {
        font.push_str(el);
    }
    font.push_str("</font>");
    font
}

/// An attribute's raw value in one open tag.
fn tag_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let (_, s, e, _) = attr_span(tag, name)?;
    Some(&tag[s..e])
}

/// Where attribute `name` sits in one open tag: the whitespace before it,
/// its value's start and end, and the end past the closing quote. Any
/// whitespace may surround the `=`, and the value may be in either quote.
fn attr_span(tag: &str, name: &str) -> Option<(usize, usize, usize, usize)> {
    let b = tag.as_bytes();
    let mut from = 0;
    while let Some(off) = tag[from..].find(name) {
        let at = from + off;
        from = at + name.len();
        let before = at.checked_sub(1).map(|i| b[i]);
        if !before.is_some_and(|c| c.is_ascii_whitespace()) {
            continue;
        }
        let mut i = at + name.len();
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if b.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let Some(&q) = b.get(i).filter(|c| matches!(c, b'"' | b'\'')) else {
            continue;
        };
        let start = i + 1;
        let len = tag[start..].find(q as char)?;
        let mut ws = at;
        while ws > 0 && b[ws - 1].is_ascii_whitespace() {
            ws -= 1;
        }
        return Some((ws, start, start + len, start + len + 1));
    }
    None
}

/// Every `name="value"` of one open tag, raw, in order.
fn tag_attrs(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = tag
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim_end_matches('/');
    rest = rest.split_once(char::is_whitespace).map_or("", |(_, r)| r);
    while let Some(eq) = rest.find("=\"") {
        let name = rest[..eq].trim().to_string();
        let v0 = eq + 2;
        let Some(v1) = rest[v0..].find('"').map(|e| v0 + e) else {
            break;
        };
        out.push((name, rest[v0..v1].to_string()));
        rest = &rest[v1 + 1..];
    }
    out
}

/// A child element of an `<xf>` as written (self-closing or not), e.g. its
/// `<alignment …/>` or `<protection …/>`.
fn child_open_tag(xf: &str, name: &str) -> Option<String> {
    let s = xf.find(&format!("<{name}"))?;
    let tag_end = s + xf[s..].find('>')? + 1;
    if xf[..tag_end].ends_with("/>") {
        return Some(xf[s..tag_end].to_string());
    }
    let close = format!("</{name}>");
    let e = tag_end + xf[tag_end..].find(&close)? + close.len();
    Some(xf[s..e].to_string())
}

/// The largest `numFmtId` used anywhere (custom ids start at 164).
fn max_numfmt_id(xml: &str) -> u32 {
    let mut max = 163u32;
    let mut i = 0;
    while let Some(p) = xml[i..].find("numFmtId=\"") {
        let s = i + p + 10;
        let e = xml[s..].find('"').map(|x| s + x).unwrap_or(s);
        if let Ok(n) = xml[s..e].parse::<u32>() {
            max = max.max(n);
        }
        i = e;
    }
    max
}

/// Append the authored `xfs` to the original `styles.xml`, leaving every
/// existing style byte-for-byte intact. An xf derived from a loaded one
/// (`Xf::loaded_from`) reuses that source `<xf>`'s font, fill, border and
/// number format wherever the modeled fields still match it, and keeps its
/// alignment attributes, protection and `xfId`; only what an edit changed is
/// minted fresh (a font, a solid fill, a thin box border, a custom numFmt).
/// An xf built from scratch mints all of its parts.
fn splice_styles(orig: &str, authored: &[Xf]) -> String {
    if authored.is_empty() {
        return orig.to_string();
    }
    let font_base = read_count(orig, "<fonts");
    let fill_base = read_count(orig, "<fills");
    let border_base = read_count(orig, "<borders");
    let mut next_numfmt = max_numfmt_id(orig) + 1;

    let (mut new_fonts, mut new_fills, mut new_borders, mut new_numfmts, mut new_xfs) = (
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    );
    let (mut fonts_added, mut fills_added, mut borders_added, mut numfmts_added) =
        (0u32, 0u32, 0u32, 0u32);

    // Excel accepts an integer point size without a trailing ".0".
    let fmt_size = |s: f64| {
        if s.fract() == 0.0 {
            format!("{}", s as i64)
        } else {
            format!("{s}")
        }
    };

    // The loaded `<xf>` elements, to derive from (see `Xf::loaded_from`).
    // Only trusted when they line up one-to-one with what the parser read.
    let src_parsed = parse_styles(orig).xfs;
    let src_raw = cell_xf_elements(orig);
    let sources_ok = src_raw.len() == src_parsed.len();
    let src_fonts = font_elements(orig);

    for xf in authored {
        let source = xf
            .loaded_from
            .filter(|_| sources_ok)
            .and_then(|i| Some((src_parsed.get(i as usize)?, src_raw.get(i as usize)?)));
        let open = |raw: &str| raw[..raw.find('>').map_or(raw.len(), |e| e + 1)].to_string();
        let src_id = |attr: &str| -> Option<u32> {
            source.and_then(|(_, raw)| tag_attr(&open(raw), attr)?.parse().ok())
        };

        // Font: the source's while bold/italic/colour/size/name still match
        // (its underline and the rest come along), else a fresh one: the
        // source font with only the changed children rewritten, or one built
        // from scratch when there is no source font.
        let same_font = source.is_some_and(|(sx, _)| {
            (sx.bold, sx.italic, sx.color, sx.font_size, &sx.font_name)
                == (xf.bold, xf.italic, xf.color, xf.font_size, &xf.font_name)
        });
        let src_font = source.and_then(|(sx, _)| {
            let raw = src_fonts.get(src_id("fontId")? as usize)?;
            Some((sx, *raw))
        });
        let font_id = match (src_id("fontId").filter(|_| same_font), src_font) {
            (Some(id), _) => id,
            (None, Some((sx, raw))) => {
                new_fonts.push_str(&edit_font(raw, sx, xf, fmt_size));
                let id = font_base + fonts_added;
                fonts_added += 1;
                id
            }
            (None, None) => {
                let mut font = String::from("<font>");
                if xf.bold {
                    font.push_str("<b/>");
                }
                if xf.italic {
                    font.push_str("<i/>");
                }
                if let Some((r, g, b)) = xf.color {
                    font.push_str(&format!("<color rgb=\"FF{r:02X}{g:02X}{b:02X}\"/>"));
                }
                font.push_str(&format!(
                    "<sz val=\"{}\"/><name val=\"{}\"/></font>",
                    fmt_size(xf.font_size.unwrap_or(11.0)),
                    esc_attr(xf.font_name.as_deref().unwrap_or("Calibri"))
                ));
                let id = font_base + fonts_added;
                new_fonts.push_str(&font);
                fonts_added += 1;
                id
            }
        };

        // Fill: the source's while the solid colour still matches.
        let same_fill = source.is_some_and(|(sx, _)| sx.fill == xf.fill);
        let fill_id = match src_id("fillId").filter(|_| same_fill) {
            Some(id) => id,
            None => {
                if let Some((r, g, b)) = xf.fill {
                    new_fills.push_str(&format!(
                        "<fill><patternFill patternType=\"solid\"><fgColor rgb=\"FF{r:02X}{g:02X}{b:02X}\"/><bgColor indexed=\"64\"/></patternFill></fill>"
                    ));
                    let id = fill_base + fills_added;
                    fills_added += 1;
                    id
                } else {
                    0
                }
            }
        };

        // Border: the source's while the box flag still matches.
        let same_border = source.is_some_and(|(sx, _)| sx.border == xf.border);
        let border_id = match src_id("borderId").filter(|_| same_border) {
            Some(id) => id,
            None => {
                if xf.border {
                    new_borders.push_str(
                        "<border><left style=\"thin\"/><right style=\"thin\"/><top style=\"thin\"/><bottom style=\"thin\"/><diagonal/></border>",
                    );
                    let id = border_base + borders_added;
                    borders_added += 1;
                    id
                } else {
                    0
                }
            }
        };

        // Number format: the source's id while the code still matches, else
        // a custom one.
        let same_num = source.is_some_and(|(sx, _)| sx.code == xf.code);
        let num_id = match src_id("numFmtId").filter(|_| same_num) {
            Some(id) => id,
            None => {
                if let Some(code) = &xf.code {
                    let id = next_numfmt;
                    next_numfmt += 1;
                    numfmts_added += 1;
                    new_numfmts.push_str(&format!(
                        "<numFmt numFmtId=\"{id}\" formatCode=\"{}\"/>",
                        esc_attr(code)
                    ));
                    id
                } else {
                    0
                }
            }
        };
        let xf_id = src_id("xfId").unwrap_or(0);

        let mut x = format!(
            "<xf numFmtId=\"{num_id}\" fontId=\"{font_id}\" fillId=\"{fill_id}\" borderId=\"{border_id}\" xfId=\"{xf_id}\" applyFont=\"1\""
        );
        if num_id != 0 {
            x.push_str(" applyNumberFormat=\"1\"");
        }
        if fill_id != 0 {
            x.push_str(" applyFill=\"1\"");
        }
        if border_id != 0 {
            x.push_str(" applyBorder=\"1\"");
        }
        if xf.quote_prefix {
            x.push_str(" quotePrefix=\"1\"");
        }

        // Alignment: the source's attributes (vertical, indent, rotation, a
        // horizontal the model has no name for such as centerContinuous…),
        // with horizontal / wrapText rewritten only where the edit changed
        // them.
        let mut align: Vec<(String, String)> = source
            .and_then(|(_, raw)| child_open_tag(raw, "alignment"))
            .map(|tag| tag_attrs(&tag))
            .unwrap_or_default();
        let (align_changed, wrap_changed) = match source {
            Some((sx, _)) => (sx.align != xf.align, sx.wrap != xf.wrap),
            None => (true, true),
        };
        if align_changed {
            align.retain(|(k, _)| k != "horizontal");
            if let Some(a) = xf.align.attr() {
                align.push(("horizontal".into(), a.to_string()));
            }
        }
        if wrap_changed {
            align.retain(|(k, _)| k != "wrapText");
            if xf.wrap {
                align.push(("wrapText".into(), "1".into()));
            }
        }
        let protection = source.and_then(|(_, raw)| child_open_tag(raw, "protection"));
        if protection.is_some() {
            x.push_str(" applyProtection=\"1\"");
        }
        if align.is_empty() && protection.is_none() {
            x.push_str("/>");
        } else {
            if !align.is_empty() {
                x.push_str(" applyAlignment=\"1\"");
            }
            x.push('>');
            if !align.is_empty() {
                x.push_str("<alignment");
                for (k, v) in &align {
                    x.push_str(&format!(" {k}=\"{v}\""));
                }
                x.push_str("/>");
            }
            if let Some(p) = protection {
                x.push_str(&p);
            }
            x.push_str("</xf>");
        }
        new_xfs.push_str(&x);
    }

    let mut xml = orig.to_string();
    // numFmts (create the container if the file has none).
    if numfmts_added > 0 {
        if xml.contains("<numFmts") {
            xml = bump_count(&xml, "<numFmts", numfmts_added);
            xml = xml.replacen("</numFmts>", &format!("{new_numfmts}</numFmts>"), 1);
        } else {
            let block = format!("<numFmts count=\"{numfmts_added}\">{new_numfmts}</numFmts>");
            xml = xml.replacen("<fonts", &format!("{block}<fonts"), 1);
        }
    }
    xml = bump_count(&xml, "<fonts", fonts_added);
    xml = xml.replacen("</fonts>", &format!("{new_fonts}</fonts>"), 1);
    if fills_added > 0 {
        xml = bump_count(&xml, "<fills", fills_added);
        xml = xml.replacen("</fills>", &format!("{new_fills}</fills>"), 1);
    }
    if borders_added > 0 {
        xml = bump_count(&xml, "<borders", borders_added);
        xml = xml.replacen("</borders>", &format!("{new_borders}</borders>"), 1);
    }
    xml = bump_count(&xml, "<cellXfs", authored.len() as u32);
    xml = xml.replacen("</cellXfs>", &format!("{new_xfs}</cellXfs>"), 1);
    xml
}

/// One worksheet: `<sheetData>`, `<cols>`, `<mergeCells>`; everything else is
/// preserved through the source-splice on save.
fn parse_worksheet(
    xml: &str,
    shared: &[String],
    hlink_targets: &std::collections::HashMap<String, String>,
) -> Sheet {
    let mut sheet = Sheet::default();
    let mut p = XmlParser::new(xml);

    // Shared-formula masters: si → (row, col, source).
    let mut shared_masters: BTreeMap<u32, (u32, u32, String)> = BTreeMap::new();
    // Followers to fill in after the pass: (row, col, si).
    let mut followers: Vec<(u32, u32, u32)> = Vec::new();

    let mut cur_row: u32 = 0;
    let mut next_col: u32 = 0;

    // Conditional-formatting parse state.
    use crate::sheet::{CfKind, CfRule, CondFormat};
    let mut cur_cf: Option<CondFormat> = None;
    // (type, operator, dxfId, priority) of the rule being read.
    let mut cf_rule: Option<(String, String, Option<usize>, i32)> = None;
    let mut cf_formulas: Vec<String> = Vec::new();
    // An `<iconSet>` rule's set, `reverse`, and `<cfvo>` thresholds.
    let mut cf_icons: Option<(String, bool, Vec<crate::sheet::Cfvo>)> = None;
    let mut in_cf_formula = false;
    let mut cf_formula_buf = String::new();

    // Where the worksheet's top-level blocks stand, as the save finds them, so
    // each one loaded from there knows its element ([`crate::sheet::CondFormat::ix`]).
    let cf_spans = cond_format_spans(xml);
    let dv_spans = validation_spans(xml)
        .map(|(_, items)| items)
        .unwrap_or_default();

    // Data-validation parse state.
    let mut cur_dv: Option<crate::sheet::DataValidation> = None;
    let mut dv_formula: u8 = 0; // 0 = none, 1 = formula1, 2 = formula2
    let mut dv_buf = String::new();

    // Inside the sheet's own `<rowBreaks>` (true) or `<colBreaks>` (false).
    // A custom view (`<customSheetView>`) carries breaks of its own under the
    // same names; those are the view's, not the sheet's, and stay verbatim.
    let mut in_breaks: Option<bool> = None;
    let mut in_custom_views = false;
    // The sheet's freeze is its first top-level `<sheetView>`'s `<pane>`, the
    // one the writer (`first_sheet_view`) rewrites. A second view (another
    // workbook window) or a custom view keeps a pane of its own.
    let mut sheet_views_seen = 0u32;
    let mut in_first_view = false;

    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "col" => {
                    let min: u32 = p.attr("min").parse().unwrap_or(1);
                    let max: u32 = p.attr("max").parse().unwrap_or(min);
                    let width = p.attr("width").parse::<f64>().ok();
                    // A width that is just the sheet's default, not marked
                    // custom, is the default: it shows like its neighbours
                    // and is written back as it was.
                    let custom = matches!(p.attr("customWidth").trim(), "1" | "true");
                    let default_width = !custom && width == Some(sheet.default_col_file_width());
                    let width = if default_width { None } else { width };
                    let mut attrs = String::new();
                    for a in p.attrs() {
                        if !matches!(a.name, "min" | "max" | "width" | "customWidth") {
                            attrs.push(' ');
                            attrs.push_str(a.name);
                            attrs.push_str("=\"");
                            attrs.push_str(&esc_raw_attr(a.value));
                            attrs.push('"');
                        }
                    }
                    sheet.col_defs.push(ColDef {
                        min: min.saturating_sub(1),
                        max: max.saturating_sub(1),
                        width,
                        attrs,
                        default_width,
                    });
                }
                "row" => {
                    // `r` is 1-based; a crafted `r="0"` must not underflow.
                    cur_row = p
                        .attr("r")
                        .parse::<u32>()
                        .map(|r| r.saturating_sub(1))
                        .unwrap_or(cur_row);
                    next_col = 0;
                    let mut attrs = String::new();
                    for a in p.attrs() {
                        if !matches!(a.name, "r" | "spans") {
                            attrs.push(' ');
                            attrs.push_str(a.name);
                            attrs.push_str("=\"");
                            attrs.push_str(&esc_raw_attr(a.value));
                            attrs.push('"');
                        }
                    }
                    if !attrs.is_empty() {
                        sheet.row_attrs.insert(cur_row, attrs);
                    }
                }
                "c" => {
                    let (row, col) = match parse_cell_name(p.attr("r")) {
                        Some(rc) => rc,
                        None => (cur_row, next_col),
                    };
                    next_col = col + 1;
                    let style: u32 = p.attr("s").parse().unwrap_or(0);
                    let ctype = p.attr("t").to_string();
                    let (cm, vm) = (p.attr("cm").to_string(), p.attr("vm").to_string());
                    let ph = matches!(p.attr("ph"), "1" | "true");
                    let mut cell = parse_cell_body(
                        &mut p,
                        &ctype,
                        style,
                        shared,
                        row,
                        col,
                        &mut shared_masters,
                        &mut followers,
                    );
                    // `cm` is the dynamic-array marker: meaningless (and never
                    // written) on anything but an array `<f>`.
                    let array_f = cell.f_attrs.as_deref().is_some_and(is_array_f);
                    let meta = CellMeta {
                        cm: (!cm.is_empty() && array_f).then_some(cm),
                        vm: (!vm.is_empty()).then(|| (vm, cell.value.clone())),
                        ph,
                        ..CellMeta::default()
                    };
                    if meta != CellMeta::default() {
                        cell.meta = Some(Box::new(meta));
                    }
                    if !(cell.is_blank() && cell.style == 0 && cell.f_attrs.is_none()) {
                        sheet.cells.insert((row, col), cell);
                    }
                }
                "mergeCell" => {
                    if let Some(rect) = parse_range_name(p.attr("ref")) {
                        sheet.merges.push(rect);
                    }
                }
                // A frozen pane: the leading `ySplit` rows / `xSplit` cols stay put.
                "sheetView" if !in_custom_views => {
                    in_first_view = sheet_views_seen == 0;
                    sheet_views_seen += 1;
                }
                "pane" if in_first_view && !in_custom_views => {
                    if matches!(p.attr("state"), "frozen" | "frozenSplit") {
                        let cols = p.attr("xSplit").parse::<u32>().unwrap_or(0);
                        let rows = p.attr("ySplit").parse::<u32>().unwrap_or(0);
                        sheet.freeze = (rows, cols);
                    }
                }
                "customSheetViews" => in_custom_views = true,
                // The defaults rows and columns without their own size take.
                "sheetFormatPr" if !in_custom_views => {
                    let num = |a: &str| {
                        p.attr(a)
                            .trim()
                            .parse::<f64>()
                            .ok()
                            .filter(|v| v.is_finite() && *v >= 0.0)
                    };
                    sheet.format = crate::sheet::SheetFormat {
                        default_col_width: num("defaultColWidth"),
                        base_col_width: num("baseColWidth").map_or(8, |v| v as u32),
                        default_row_height: num("defaultRowHeight"),
                    };
                }
                "rowBreaks" if !in_custom_views => in_breaks = Some(true),
                "colBreaks" if !in_custom_views => in_breaks = Some(false),
                "brk" => match in_breaks {
                    Some(true) => sheet.row_breaks.extend(page_break(&p)),
                    Some(false) => sheet.col_breaks.extend(page_break(&p)),
                    None => {}
                },
                // Sheet protection: preserve the whole flag/password attribute set
                // verbatim so it round-trips untouched.
                "sheetProtection" => {
                    let mut attrs = String::new();
                    for a in p.attrs() {
                        if !attrs.is_empty() {
                            attrs.push(' ');
                        }
                        attrs.push_str(a.name);
                        attrs.push_str("=\"");
                        attrs.push_str(&esc_raw_attr(a.value));
                        attrs.push('"');
                    }
                    sheet.protection = Some(attrs);
                }
                // A cell hyperlink: `ref` cell/range → external URL (via r:id) or
                // an in-workbook `location`.
                "hyperlink" => {
                    let ref_attr = p.attr("ref").to_string();
                    let rid = p.attr("r:id").to_string();
                    let location = p.attr("location").to_string();
                    if let Some((r1, c1, r2, c2)) = parse_range_name(&ref_attr)
                        .or_else(|| parse_cell_name(&ref_attr).map(|(r, c)| (r, c, r, c)))
                    {
                        let target = if !rid.is_empty() {
                            hlink_targets.get(&rid).cloned()
                        } else if !location.is_empty() {
                            Some(format!("#{location}"))
                        } else {
                            None
                        };
                        if let Some(t) = target {
                            sheet.hyperlink_refs.insert((r1, c1), (r1, c1, r2, c2));
                            // A whole-column hyperlink applies only to its anchor.
                            let cells = (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64;
                            if cells > 4096 {
                                sheet.hyperlinks.insert((r1, c1), t);
                            } else {
                                for r in r1..=r2 {
                                    for c in c1..=c2 {
                                        sheet.hyperlinks.insert((r, c), t.clone());
                                    }
                                }
                            }
                        }
                    }
                }
                // Conditional formatting: a block of rules over `sqref` ranges.
                "conditionalFormatting" => {
                    let mut ranges = Vec::new();
                    for tok in p.attr("sqref").split_whitespace() {
                        if let Some(r) = parse_range_name(tok)
                            .or_else(|| parse_cell_name(tok).map(|(r, c)| (r, c, r, c)))
                        {
                            ranges.push(r);
                        }
                    }
                    let start = p.start_pos();
                    cur_cf = Some(CondFormat {
                        ranges,
                        rules: Vec::new(),
                        ix: cf_spans.iter().position(|&(s, _)| s == start),
                    });
                }
                "cfRule" if cur_cf.is_some() => {
                    cf_rule = Some((
                        p.attr("type").to_string(),
                        p.attr("operator").to_string(),
                        p.attr("dxfId").parse::<usize>().ok(),
                        p.attr("priority").parse::<i32>().unwrap_or(0),
                    ));
                    cf_formulas.clear();
                    cf_icons = None;
                }
                "iconSet" if cf_rule.is_some() => {
                    let set = match p.attr("iconSet") {
                        "" => "3TrafficLights1",
                        s => s,
                    };
                    cf_icons = Some((
                        set.to_string(),
                        matches!(p.attr("reverse"), "1" | "true"),
                        Vec::new(),
                    ));
                }
                "cfvo" if cf_icons.is_some() => {
                    if let Some((_, _, cfvos)) = cf_icons.as_mut() {
                        cfvos.push(crate::sheet::Cfvo {
                            kind: p.attr("type").to_string(),
                            val: decode(p.attr("val")),
                            gte: !matches!(p.attr("gte"), "0" | "false"),
                        });
                    }
                }
                "formula" if cf_rule.is_some() => {
                    in_cf_formula = true;
                    cf_formula_buf.clear();
                }
                // Data validation: a constraint (list/number/date/…) over `sqref`.
                "dataValidation" => {
                    let mut ranges = Vec::new();
                    for tok in p.attr("sqref").split_whitespace() {
                        if let Some(r) = parse_range_name(tok)
                            .or_else(|| parse_cell_name(tok).map(|(r, c)| (r, c, r, c)))
                        {
                            ranges.push(r);
                        }
                    }
                    let pr = p.attr("prompt");
                    let prompt = (!pr.is_empty()).then(|| decode(pr));
                    let start = p.start_pos();
                    cur_dv = Some(crate::sheet::DataValidation {
                        ranges,
                        kind: p.attr("type").to_string(),
                        operator: p.attr("operator").to_string(),
                        formula1: String::new(),
                        formula2: String::new(),
                        prompt,
                        prompt_title: decode(p.attr("promptTitle")),
                        allow_blank: flag_attr(p.attr("allowBlank")),
                        show_input: flag_attr(p.attr("showInputMessage")),
                        show_error: flag_attr(p.attr("showErrorMessage")),
                        show_dropdown: !flag_attr(p.attr("showDropDown")),
                        error_style: crate::sheet::AlertStyle::from_attr(p.attr("errorStyle")),
                        error_title: decode(p.attr("errorTitle")),
                        error: decode(p.attr("error")),
                        ix: dv_spans.iter().position(|&(s, _)| s == start),
                        orig: None,
                    });
                }
                "formula1" if cur_dv.is_some() => {
                    dv_formula = 1;
                    dv_buf.clear();
                }
                "formula2" if cur_dv.is_some() => {
                    dv_formula = 2;
                    dv_buf.clear();
                }
                _ => {}
            },
            Event::Text => {
                if in_cf_formula {
                    cf_formula_buf.push_str(p.text());
                }
                if dv_formula > 0 {
                    dv_buf.push_str(p.text());
                }
            }
            Event::End => match local(p.name()) {
                "row" => cur_row += 1,
                "rowBreaks" | "colBreaks" => in_breaks = None,
                "customSheetViews" => in_custom_views = false,
                "sheetView" => in_first_view = false,
                "formula" if in_cf_formula => {
                    in_cf_formula = false;
                    cf_formulas.push(decode(&std::mem::take(&mut cf_formula_buf)));
                }
                "cfRule" => {
                    if let (Some((ty, op, dxf_id, priority)), Some(cf)) =
                        (cf_rule.take(), cur_cf.as_mut())
                    {
                        let kind = match ty.as_str() {
                            "cellIs" => CfKind::CellIs {
                                op,
                                formulas: std::mem::take(&mut cf_formulas),
                            },
                            "expression" => CfKind::Expression {
                                formula: cf_formulas.first().cloned().unwrap_or_default(),
                            },
                            "iconSet" if cf_icons.is_some() => {
                                let (set, reverse, cfvos) = cf_icons.take().unwrap_or_default();
                                CfKind::IconSet {
                                    set,
                                    reverse,
                                    cfvos,
                                    formulas: std::mem::take(&mut cf_formulas),
                                }
                            }
                            _ => CfKind::Other {
                                rule_type: ty.clone(),
                                formulas: std::mem::take(&mut cf_formulas),
                            },
                        };
                        cf.rules.push(CfRule {
                            kind,
                            dxf_id,
                            priority,
                        });
                    }
                    cf_formulas.clear();
                }
                "conditionalFormatting" => {
                    if let Some(cf) = cur_cf.take() {
                        if !cf.rules.is_empty() {
                            sheet.cond_formats.push(cf);
                        }
                    }
                }
                "formula1" if dv_formula == 1 => {
                    dv_formula = 0;
                    if let Some(dv) = cur_dv.as_mut() {
                        dv.formula1 = decode(&std::mem::take(&mut dv_buf));
                    }
                }
                "formula2" if dv_formula == 2 => {
                    dv_formula = 0;
                    if let Some(dv) = cur_dv.as_mut() {
                        dv.formula2 = decode(&std::mem::take(&mut dv_buf));
                    }
                }
                "dataValidation" => {
                    if let Some(mut dv) = cur_dv.take() {
                        if !dv.ranges.is_empty() && dv.is_meaningful() {
                            dv.orig = Some(Box::new(dv.clone()));
                            sheet.validations.push(dv);
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => break,
        }
    }

    // Expand shared-formula followers from their master, shifting relative
    // refs by the offset. If the master doesn't parse, preserve the group
    // verbatim (master keeps its text; followers keep the si marker).
    for (row, col, si) in followers {
        let Some((mr, mc, src)) = shared_masters.get(&si) else {
            continue;
        };
        let dr = row as i64 - *mr as i64;
        let dc = col as i64 - *mc as i64;
        let translated = translate_formula(src, dr, dc);
        if let Some(cell) = sheet.cells.get_mut(&(row, col)) {
            match &translated {
                Some(f) => cell.formula = Some(f.clone()),
                None => {
                    cell.formula = Some(String::new());
                    cell.f_attrs = Some(format!(" t=\"shared\" si=\"{si}\""));
                }
            }
        }
        if translated.is_none() {
            // Master keeps its original shared attrs too.
            if let Some(mcell) = sheet.cells.get_mut(&(*mr, *mc)) {
                if mcell.f_attrs.is_none() {
                    mcell.f_attrs = Some(format!(" t=\"shared\" si=\"{si}\""));
                }
            }
        }
    }
    // Masters of *parseable* groups become plain formulas (their f_attrs
    // were never set), which is what we write back — Excel accepts expanded
    // formulas in place of shared groups.
    cap_array_refs(&mut sheet);
    sheet.page_setup = page::read_page_setup(xml);
    sheet.page_setup_loaded = sheet.page_setup.clone();
    sheet.outline = page::read_outline_pr(xml);
    sheet.outline_loaded = sheet.outline;
    sheet.consolidate = consolidate::read(xml);
    sheet.consolidate_loaded = sheet.consolidate.clone();
    sheet
}

/// Block cells a sheet may name through array `ref`s beyond twice its own
/// cell count ([`cap_array_refs`]).
const ARRAY_REF_SLACK: u64 = 1 << 16;

/// Hold the array blocks a loaded sheet names to what the file can back:
/// Excel writes every cell of a legacy CSE block and of a spilled dynamic
/// array, so a genuine sheet's blocks cover at most its own cells. Each
/// array `ref` that starts at its cell (the ones the engine fills or spills
/// over, and save checks) spends its area from one per-sheet budget of twice
/// the cell count plus [`ARRAY_REF_SLACK`]; one that doesn't fit is corrupt or
/// crafted (`ref="A1:XFD1048576"`), and its cell falls back to a one-cell
/// array, keeping its formula and cached value. A loaded extent
/// ([`Cell::spill`]) is kept only where it is exactly such a block, so the
/// budget bounds it too, whatever produced it. Every later walk over a block
/// or extent is then bounded by the sheet's size.
///
/// What is left: a ref within the slack still has its cells made on its
/// first fill (up to 64Ki), and since refs spend in key order, refs early in
/// a sheet can leave a later block to fall back. Beyond a crafted file, that
/// happens to a block larger than the slack from a writer that saves only
/// its anchor cell (openpyxl's `ArrayFormula`, for one): it loads, and then
/// saves, as a one-cell array.
fn cap_array_refs(sheet: &mut Sheet) {
    let mut budget = 2 * sheet.cells.len() as u64 + ARRAY_REF_SLACK;
    for (&(row, col), cell) in sheet.cells.iter_mut() {
        // The block a `ref` that starts at the cell names, as (rows, cols).
        let own = crate::sheet::array_block(cell)
            .filter(|&(r1, c1, _, _)| (r1, c1) == (row, col))
            .map(|(r1, c1, r2, c2)| (r2 - r1 + 1, c2 - c1 + 1));
        if cell.spill.is_some() && cell.spill != own {
            cell.spill = None;
        }
        let Some((h, w)) = own else {
            continue;
        };
        let area = u64::from(h) * u64::from(w);
        if area <= 1 {
            continue;
        }
        if area <= budget {
            budget -= area;
            continue;
        }
        if let Some(fa) = cell.f_attrs.as_deref() {
            cell.f_attrs = Some(with_ref(fa, &cell_name(row, col)));
        }
        cell.spill = None;
    }
}

/// Parse the children of one `<c>` (consumes through `</c>`).
#[allow(clippy::too_many_arguments)]
fn parse_cell_body(
    p: &mut XmlParser<'_>,
    ctype: &str,
    style: u32,
    shared: &[String],
    row: u32,
    col: u32,
    shared_masters: &mut BTreeMap<u32, (u32, u32, String)>,
    followers: &mut Vec<(u32, u32, u32)>,
) -> Cell {
    let mut v_text: Option<String> = None;
    let mut is_text: Option<String> = None; // inline string content
    let mut formula: Option<String> = None;
    let mut f_attrs: Option<String> = None;
    let mut depth = 1;
    let mut in_v = false;
    let mut in_f = false;
    let mut in_is_t = false;
    while depth > 0 {
        match p.next() {
            Event::Start => {
                depth += 1;
                match local(p.name()) {
                    "v" => {
                        in_v = true;
                        v_text = Some(String::new());
                    }
                    "f" => {
                        in_f = true;
                        formula = Some(String::new());
                        let t = p.attr("t").to_string();
                        let si = p.attr("si").to_string();
                        let ref_attr = p.attr("ref").to_string();
                        match t.as_str() {
                            "shared" => {
                                // Master carries text (captured below);
                                // follower carries none. Record both.
                                if let Ok(si) = si.parse::<u32>() {
                                    followers.push((row, col, si));
                                    // Only the master carries a `ref=` span;
                                    // seed the group's source cell from it. A
                                    // follower seen before its master must not
                                    // claim the slot (which would leave the
                                    // group's source empty), so key on `ref`.
                                    if !ref_attr.is_empty() {
                                        shared_masters.insert(si, (row, col, String::new()));
                                    }
                                }
                            }
                            "" | "normal" => {}
                            _ => {
                                // array / dataTable — preserve verbatim.
                                let mut attrs = String::new();
                                for a in p.attrs() {
                                    attrs.push(' ');
                                    attrs.push_str(a.name);
                                    attrs.push_str("=\"");
                                    attrs.push_str(&esc_raw_attr(a.value));
                                    attrs.push('"');
                                }
                                f_attrs = Some(attrs);
                            }
                        }
                    }
                    "t" => in_is_t = true,
                    "rPh" => {
                        p.skip_element();
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            Event::Text => {
                if in_v {
                    if let Some(s) = &mut v_text {
                        XmlParser::append_decoded(p.text(), s);
                    }
                } else if in_f {
                    if let Some(s) = &mut formula {
                        XmlParser::append_decoded(p.text(), s);
                    }
                } else if in_is_t {
                    let s = is_text.get_or_insert_with(String::new);
                    XmlParser::append_decoded(p.text(), s);
                }
            }
            Event::End => {
                depth -= 1;
                match local(p.name()) {
                    "v" => in_v = false,
                    "f" => {
                        in_f = false;
                        // A shared master's text registers the group source.
                        if let Some(src) = &formula {
                            if !src.is_empty() {
                                for m in shared_masters.values_mut() {
                                    if m.0 == row && m.1 == col && m.2.is_empty() {
                                        m.2 = src.clone();
                                    }
                                }
                            }
                        }
                    }
                    "t" => in_is_t = false,
                    _ => {}
                }
            }
            Event::Eof => break,
        }
    }

    // Follower cells have an empty <f/>: represent as "no formula yet"; the
    // expansion pass fills them in.
    let formula = match formula {
        Some(f) if f.is_empty() && f_attrs.is_none() => Some(String::new()),
        other => other,
    };

    let value = if let Some(t) = is_text {
        CellValue::Text(t)
    } else {
        match (ctype, v_text) {
            (_, None) => CellValue::Empty,
            ("s", Some(v)) => {
                let idx: usize = v.trim().parse().unwrap_or(usize::MAX);
                CellValue::Text(shared.get(idx).cloned().unwrap_or_default())
            }
            ("str", Some(v)) => CellValue::Text(v),
            ("b", Some(v)) => CellValue::Bool(v.trim() == "1" || v.trim() == "true"),
            ("e", Some(v)) => CellValue::Error(v.trim().to_string()),
            ("d", Some(v)) => CellValue::Text(v),
            (_, Some(v)) => match v.trim().parse::<f64>() {
                Ok(n) => CellValue::Number(n),
                Err(_) => CellValue::Text(v),
            },
        }
    };

    // An array formula's `ref` records its extent: a dynamic array's spill,
    // which the engine re-derives on recalculation, or a legacy CSE block,
    // which the engine refills.
    let spill = f_attrs.as_deref().and_then(|a| {
        if !is_array_f(a) {
            return None;
        }
        let ref_val = crate::sheet::f_ref(a)?;
        let (r1, c1, r2, c2) = crate::sheet::parse_range_name(ref_val)?;
        if (r1, c1) != (row, col) {
            return None;
        }
        Some((r2 - r1 + 1, c2 - c1 + 1))
    });

    Cell {
        value,
        formula,
        f_attrs,
        style,
        spill,
        meta: None,
    }
}

/// The `<brk>` the parser is on; `None` when its `id` is unreadable.
fn page_break(p: &XmlParser) -> Option<crate::sheet::PageBreak> {
    let id = p.attr("id").parse().ok()?;
    let mut attrs = String::new();
    for a in p.attrs().iter().filter(|a| a.name != "id") {
        attrs.push(' ');
        attrs.push_str(a.name);
        attrs.push_str("=\"");
        attrs.push_str(&esc_raw_attr(a.value));
        attrs.push('"');
    }
    Some(crate::sheet::PageBreak { id, attrs })
}

/// Local name (strip any namespace prefix).
pub(crate) fn local(name: &str) -> &str {
    match name.rfind(':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

pub(crate) fn decode(raw: &str) -> String {
    let mut s = String::new();
    XmlParser::append_decoded(raw, &mut s);
    s
}

// ---------------------------------------------------------------------------
// Save
// ---------------------------------------------------------------------------

/// Can this character appear in XML 1.0 at all? Most C0 controls cannot - not
/// even as a numeric entity - so the only way to keep a part well-formed is to
/// leave them out. `=CHAR(1)` reaches the model, and our own loader is lenient
/// enough to read one straight back, so nothing else catches this.
fn xml_writable(ch: char) -> bool {
    match ch {
        '\t' | '\n' | '\r' => true,
        c if (c as u32) < 0x20 => false,
        '\u{fffe}' | '\u{ffff}' => false,
        _ => true,
    }
}

pub(crate) fn esc_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            // A literal CR would be normalized to LF on the way back in, so it
            // only survives as an entity.
            '\r' => out.push_str("&#13;"),
            c if !xml_writable(c) => {}
            _ => out.push(ch),
        }
    }
    out
}

/// Full-precision float for `<v>` (must round-trip; display formatting is a
/// separate concern).
fn num_repr(n: f64) -> String {
    // `NaN`/`inf` are not `xsd:double` lexical forms, and Rust's `{}` prints
    // exactly those. A load can carry one in: `parse::<f64>()` accepts them, so
    // a hand-written (or foreign-tool) `<v>NaN</v>` would round-trip out again
    // and make Excel reject the part.
    if !n.is_finite() {
        return "0".to_string();
    }
    if n == n.trunc() && n.abs() < 1e16 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Serialize the package back to `.xlsx` bytes (STORED ZIP), keeping the
/// loaded file's type: an `.xlsm` stays macro-enabled, an `.xltx` a template.
/// Saving to a named file goes through [`save_xlsx_as`] with that file's kind.
pub fn save_xlsx(pkg: &SheetPackage) -> Vec<u8> {
    write_zip(&saved_parts(pkg))
}

/// Serialize the package as `kind`: the workbook part's content type follows
/// the target file type, and a macro-free target (`.xlsx`, `.xltx`) loses the
/// VBA project, the Excel 4.0 macro sheets and dialog sheets, and the Excel
/// 4.0 names (see [`SheetPackage::macro_features`]). Excel refuses an
/// `.xlsx` that says it is a template or carries macros. `pkg` itself is
/// untouched.
pub fn save_xlsx_as(pkg: &SheetPackage, kind: SpreadsheetKind) -> Vec<u8> {
    let without_excel4_macros;
    let pkg = if !kind.allows_macros() && (pkg.has_macro_sheets() || pkg.has_macro_names()) {
        let mut copy = pkg.clone();
        copy.remove_excel4_macros();
        without_excel4_macros = copy;
        &without_excel4_macros
    } else {
        pkg
    };
    let mut parts = saved_parts(pkg);
    let wb_part = workbook_part_name(&parts);
    set_content_type_override(&mut parts, &format!("/{wb_part}"), kind.main_content_type());
    if !kind.allows_macros() {
        strip_vba_project(&mut parts);
    }
    write_zip(&parts)
}

/// [`save_xlsx_as`] with the kind of `path`'s extension; an extension that is
/// not a spreadsheet type keeps the loaded type, as [`save_xlsx`] does.
pub fn save_xlsx_for_path(pkg: &SheetPackage, path: impl AsRef<std::path::Path>) -> Vec<u8> {
    match SpreadsheetKind::from_path(path) {
        Some(kind) => save_xlsx_as(pkg, kind),
        None => save_xlsx(pkg),
    }
}

/// The four OOXML spreadsheet file types, which differ only in the workbook
/// part's content type and whether a VBA project may ride along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadsheetKind {
    /// `.xlsx`
    Workbook,
    /// `.xlsm`
    MacroWorkbook,
    /// `.xltx`
    Template,
    /// `.xltm`
    MacroTemplate,
}

impl SpreadsheetKind {
    /// The kind for a file extension (without the dot, any case).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "xlsx" => Some(Self::Workbook),
            "xlsm" => Some(Self::MacroWorkbook),
            "xltx" => Some(Self::Template),
            "xltm" => Some(Self::MacroTemplate),
            _ => None,
        }
    }

    /// The kind for a path's extension; `None` for anything else.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> Option<Self> {
        path.as_ref()
            .extension()
            .and_then(|e| e.to_str())
            .and_then(Self::from_extension)
    }

    /// The file extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Workbook => "xlsx",
            Self::MacroWorkbook => "xlsm",
            Self::Template => "xltx",
            Self::MacroTemplate => "xltm",
        }
    }

    /// The content type of the workbook part in a file of this kind.
    pub fn main_content_type(self) -> &'static str {
        match self {
            Self::Workbook => {
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"
            }
            Self::MacroWorkbook => "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
            Self::Template => {
                "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml"
            }
            Self::MacroTemplate => "application/vnd.ms-excel.template.macroEnabled.main+xml",
        }
    }

    /// Whether a file of this kind may carry a VBA project.
    pub fn allows_macros(self) -> bool {
        matches!(self, Self::MacroWorkbook | Self::MacroTemplate)
    }
}

const VBA_PROJECT_REL: &str = "http://schemas.microsoft.com/office/2006/relationships/vbaProject";
const VBA_PROJECT_CT: &str = "application/vnd.ms-office.vbaProject";

impl SheetPackage {
    /// Whether the workbook carries a VBA project (it came from an `.xlsm` or
    /// `.xltm`, and saving it as `.xlsx`/`.xltx` would drop the macros).
    pub fn has_vba_project(&self) -> bool {
        !vba_relationships(&self.parts).is_empty()
    }

    /// Drop the VBA project in memory with the same removal [`save_xlsx_as`]
    /// runs for a macro-free kind, so a later save cannot write the macros
    /// back. The workbook part's content type is left alone: [`save_xlsx_as`]
    /// and [`save_xlsx_for_path`] set it for the file they write. Returns
    /// whether there was one.
    pub fn remove_vba_project(&mut self) -> bool {
        strip_vba_project(&mut self.parts)
    }

    /// Whether the workbook has Excel 4.0 macro sheets or dialog sheets,
    /// which a macro-free file (`.xlsx`, `.xltx`) cannot carry.
    pub fn has_macro_sheets(&self) -> bool {
        !self.macro_sheet_indices().is_empty()
    }

    /// Whether workbook.xml has an Excel 4.0 name: a defined name marked
    /// `xlm`, `function` or `vbProcedure`, which a macro-free file cannot
    /// carry either, with or without a macro sheet.
    pub fn has_macro_names(&self) -> bool {
        self.part(&workbook_part_name(&self.parts))
            .is_some_and(|b| {
                !remove_macro_names(&String::from_utf8_lossy(b), &|_: &str| false)
                    .1
                    .is_empty()
            })
    }

    /// What a macro-free file (`.xlsx`, `.xltx`) written from this package
    /// loses, in the words of Excel's warning: its VB project, its Excel 4.0
    /// macro (and dialog) sheets, and its Excel 4.0 names. Empty when it
    /// loses nothing.
    pub fn macro_features(&self) -> Vec<&'static str> {
        let mut features = Vec::new();
        if self.has_vba_project() {
            features.push("VB project");
        }
        if self.has_macro_sheets() {
            features.push("Excel 4.0 macro sheets");
        }
        if self.has_macro_names() {
            features.push("Excel 4.0 function stored in defined names");
        }
        features
    }

    /// Drop the Excel 4.0 macro content from the copy [`save_xlsx_as`] writes
    /// for a macro-free kind: the macro sheets and dialog sheets, each through
    /// [`Self::remove_sheet`], and the Excel 4.0 names, which go even when
    /// there is no macro sheet: every defined name marked `xlm`, `function`
    /// or `vbProcedure`, and every name whose formula refers to a removed
    /// sheet (`Auto_Open=Macro1!$A$1`). A cell formula that refers to a
    /// removed sheet keeps its cell, with that reference as `#REF!`. A
    /// workbook of nothing but macro sheets first gains a blank worksheet, so
    /// one remains. Returns whether there was anything to drop.
    ///
    /// Only that copy: an open workbook keeps its macros, as Excel keeps
    /// them, and its sheet indices stay put.
    fn remove_excel4_macros(&mut self) -> bool {
        let doomed = self.macro_sheet_indices();
        if doomed.is_empty() && !self.has_macro_names() {
            return false;
        }
        let names: Vec<String> = doomed
            .iter()
            .map(|&i| self.workbook.sheets[i].name.clone())
            .collect();
        if !doomed.is_empty() && doomed.len() == self.workbook.sheets.len() {
            let mut n = 1;
            while self
                .workbook
                .sheets
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(&format!("Sheet{n}")))
            {
                n += 1;
            }
            self.add_sheet(&format!("Sheet{n}"));
        }
        let refers = |formula: &str| names.iter().any(|s| formula_refers_to_sheet(formula, s));
        let wb_part = workbook_part_name(&self.parts);
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == wb_part) {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            // A `localSheetId` that names no model sheet was loaded as a
            // global name; that model entry goes with its element too, or the
            // save would write it back (as a plain name) for having none.
            let sheet_count = self.workbook.sheets.len();
            let aligned = xml.matches("<sheet ").count() == sheet_count;
            let demoted = |s: Option<usize>| s.is_some_and(|k| !aligned || k >= sheet_count);
            let (xml, gone) = remove_macro_names(&xml, &refers);
            p.1 = xml.into_bytes();
            self.workbook.defined_names.retain(|d| {
                !refers(&d.formula)
                    && !gone.iter().any(|(n, s)| {
                        (*s == d.scope || (d.scope.is_none() && demoted(*s)))
                            && n.eq_ignore_ascii_case(&d.name)
                    })
            });
        }
        // Cell formulas that name a removed sheet read `#REF!`, as Excel
        // turns them.
        crate::edit::remove_sheet_refs(&mut self.workbook, &names);
        for &i in doomed.iter().rev() {
            self.remove_sheet(i);
        }
        true
    }

    /// Indices (ascending) of the sheets whose workbook relationship is an
    /// Excel 4.0 macro sheet (`xlMacrosheet`, `xlIntlMacrosheet`) or a dialog
    /// sheet.
    fn macro_sheet_indices(&self) -> Vec<usize> {
        self.sheet_rel_types()
            .iter()
            .enumerate()
            .filter(|(_, ty)| {
                ["/xlMacrosheet", "/xlIntlMacrosheet", "/dialogsheet"]
                    .iter()
                    .any(|suffix| ty.ends_with(suffix))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The workbook relationship type of each sheet's part, per
    /// `sheet_parts` index (`…/worksheet`, `…/chartsheet`, a macro or dialog
    /// sheet), or `""` when no relationship names the part.
    pub(crate) fn sheet_rel_types(&self) -> Vec<String> {
        let wb_part = workbook_part_name(&self.parts);
        let wb_dir = wb_part.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let rels: Vec<(String, String)> = self
            .part(&rels_part_name(&wb_part))
            .map(|b| parse_rels(&String::from_utf8_lossy(b)))
            .unwrap_or_default()
            .into_iter()
            .map(|(_, ty, t)| (resolve_relative(wb_dir, &t), ty))
            .collect();
        self.sheet_parts
            .iter()
            .map(|part| {
                rels.iter()
                    .find(|(target, _)| target == part)
                    .map(|(_, ty)| ty.clone())
                    .unwrap_or_default()
            })
            .collect()
    }
}

/// Whether `formula` refers to sheet `sheet` by name: `Macro1!…` or
/// `'Macro 1'!…`, case-insensitively, and not as the tail of a longer name.
///
/// Not in another workbook (`[1]Macro1!…`) or inside a string literal
/// (`"Macro1!A1"`) either.
fn formula_refers_to_sheet(formula: &str, sheet: &str) -> bool {
    let lower = formula.to_lowercase();
    // Which byte offsets lie inside a "…" literal. An escaped `""` leaves
    // and re-enters it, so plain toggling is right.
    let mut in_string = vec![false; lower.len()];
    let mut inside = false;
    for (i, c) in lower.char_indices() {
        if c == '"' {
            inside = !inside;
        }
        in_string[i] = inside;
    }
    let quoted = format!("'{}'!", sheet.replace('\'', "''")).to_lowercase();
    if lower.match_indices(&quoted).any(|(i, _)| !in_string[i]) {
        return true;
    }
    let bare = format!("{sheet}!").to_lowercase();
    lower.match_indices(&bare).any(|(i, _)| {
        !in_string[i]
            && !lower[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '\'' | ']'))
    })
}

/// Remove from workbook.xml every `<definedName>` that is an Excel 4.0 name
/// (`xlm`, `function` or `vbProcedure` set) or whose formula `refers` to a
/// removed sheet, and a `<definedNames>` left empty. Returns the XML and each
/// removed name with its `localSheetId` scope.
fn remove_macro_names(
    xml: &str,
    refers: &impl Fn(&str) -> bool,
) -> (String, Vec<(String, Option<usize>)>) {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut gone = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == "definedName" => {
                let start = p.start_pos();
                let is_true = |v: &str| v == "1" || v.eq_ignore_ascii_case("true");
                let macro_name = ["xlm", "function", "vbProcedure"]
                    .iter()
                    .any(|a| is_true(p.attr(a)));
                let name = decode(p.attr("name"));
                let scope = p.attr("localSheetId").parse::<usize>().ok();
                let mut text = String::new();
                let mut depth = 0usize;
                let end = loop {
                    match p.next() {
                        Event::Text => XmlParser::append_decoded(p.text(), &mut text),
                        Event::Start => depth += 1,
                        Event::End if depth > 0 => depth -= 1,
                        Event::End => break Some(p.pos()),
                        Event::Eof => break None,
                    }
                };
                let Some(end) = end else {
                    break;
                };
                if macro_name || refers(&text) {
                    spans.push((start, end));
                    gone.push((name, scope));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut out = xml.to_string();
    for (start, end) in spans.into_iter().rev() {
        out.replace_range(start..end, "");
    }
    for empty in ["<definedNames></definedNames>", "<definedNames/>"] {
        out = out.replace(empty, "");
    }
    (out, gone)
}

/// The workbook's VBA project relationships as (rels part, Id, resolved
/// target part). Matched on the exact Type: `…/vbaProjectSignature` shares the
/// prefix.
fn vba_relationships(parts: &[(String, Vec<u8>)]) -> Vec<(String, String, String)> {
    let wb_part = workbook_part_name(parts);
    let wb_dir = wb_part.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let rels_name = rels_part_name(&wb_part);
    let Some((_, xml)) = parts.iter().find(|(n, _)| *n == rels_name) else {
        return Vec::new();
    };
    parse_rels(&String::from_utf8_lossy(xml))
        .into_iter()
        .filter(|(_, ty, _)| ty == VBA_PROJECT_REL)
        .map(|(id, _, target)| (rels_name.clone(), id, resolve_relative(wb_dir, &target)))
        .collect()
}

/// Remove the VBA project: each part the workbook names as its vbaProject,
/// that relationship, the part's own rels and the parts they name (the
/// signatures), and their content-type Overrides. The `.bin` Default goes
/// too, unless another `.bin` part (printer settings) still relies on it.
/// A part is deleted only once its relationship is, so a relationship that
/// could not be found never dangles. Returns whether anything was removed.
fn strip_vba_project(parts: &mut Vec<(String, Vec<u8>)>) -> bool {
    let rels = vba_relationships(parts);
    let mut doomed: Vec<String> = Vec::new();
    for (rels_name, id, target) in &rels {
        let Some(p) = parts.iter_mut().find(|(n, _)| n == rels_name) else {
            continue;
        };
        let mut xml = String::from_utf8_lossy(&p.1).into_owned();
        let Some(el) = find_element_by_attr(&xml, "Relationship", "Id", |v| v == id) else {
            continue;
        };
        xml.replace_range(el.start..el.end, "");
        p.1 = xml.into_bytes();

        let own_rels = rels_part_name(target);
        let dir = target.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        if let Some((_, xml)) = parts.iter().find(|(n, _)| *n == own_rels) {
            for (_, _, t) in parse_rels(&String::from_utf8_lossy(xml)) {
                doomed.push(resolve_relative(dir, &t));
            }
        }
        doomed.push(target.clone());
        doomed.push(own_rels);
    }
    if doomed.is_empty() {
        return false;
    }
    parts.retain(|(n, _)| !doomed.contains(n));

    if let Some(i) = parts.iter().position(|(n, _)| n == "[Content_Types].xml") {
        let mut xml = String::from_utf8_lossy(&parts[i].1).into_owned();
        for part in &doomed {
            while let Some(el) = override_element(&xml, &format!("/{part}")) {
                xml.replace_range(el.start..el.end, "");
            }
        }
        // A `.bin` part left without an Override of its own still needs it.
        let bin_needs_default = parts.iter().any(|(n, _)| {
            n.to_ascii_lowercase().ends_with(".bin")
                && override_element(&xml, &format!("/{n}")).is_none()
        });
        if !bin_needs_default {
            while let Some(el) = find_element_by_attr(&xml, "Default", "ContentType", |v| {
                v.eq_ignore_ascii_case(VBA_PROJECT_CT)
            }) {
                xml.replace_range(el.start..el.end, "");
            }
        }
        parts[i].1 = xml.into_bytes();
    }
    true
}

/// Where [`find_element_by_attr`] found an element: its whole span (start tag to
/// end tag, or the self-closed tag) and the byte span of the matched
/// attribute's value.
pub(crate) struct ElementSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) value: (usize, usize),
}

/// The first element with local name `name` (any namespace prefix) whose
/// attribute `attr` has a (decoded) value `want` accepts. The same parser
/// that reads rels finds it, so quote style, whitespace, `/>` versus `></X>`,
/// and comments, CDATA and processing instructions are treated alike.
pub(crate) fn find_element_by_attr(
    xml: &str,
    name: &str,
    attr: &str,
    want: impl Fn(&str) -> bool,
) -> Option<ElementSpan> {
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == name => {
                let value = p
                    .attrs()
                    .iter()
                    .find(|a| local(a.name) == attr && want(&decode(a.value)))
                    .map(|a| {
                        let at = a.value.as_ptr() as usize - xml.as_ptr() as usize;
                        (at, at + a.value.len())
                    });
                if let Some(value) = value {
                    let start = p.start_pos();
                    // A truncated element has no span to remove or rewrite.
                    if !p.skip_element_complete() {
                        return None;
                    }
                    return Some(ElementSpan {
                        start,
                        end: p.pos(),
                        value,
                    });
                }
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// The `<Override>` whose PartName is `part_name` (OPC part names compare
/// case-insensitively).
pub(crate) fn override_element(xml: &str, part_name: &str) -> Option<ElementSpan> {
    find_element_by_attr(xml, "Override", "PartName", |v| {
        v.eq_ignore_ascii_case(part_name)
    })
}

/// Point `part_name`'s `<Override>` at `ct`, replacing whatever type it had
/// in place, or add one. (`add_content_type_override` leaves an existing
/// entry alone.)
fn set_content_type_override(parts: &mut [(String, Vec<u8>)], part_name: &str, ct: &str) {
    let Some(p) = parts.iter_mut().find(|(n, _)| n == "[Content_Types].xml") else {
        return;
    };
    let mut xml = String::from_utf8_lossy(&p.1).into_owned();
    let ov = format!("<Override PartName=\"{part_name}\" ContentType=\"{ct}\"/>");
    match override_element(&xml, part_name) {
        Some(el) => {
            let current =
                find_element_by_attr(&xml[el.start..el.end], "Override", "ContentType", |_| true);
            match current {
                Some(c) => xml.replace_range(el.start + c.value.0..el.start + c.value.1, ct),
                None => xml.replace_range(el.start..el.end, &ov),
            }
        }
        None => xml = xml.replacen("</Types>", &format!("{ov}</Types>"), 1),
    }
    p.1 = xml.into_bytes();
}

/// The parts [`save_xlsx`] writes: the originals with the model spliced in.
fn saved_parts(pkg: &SheetPackage) -> Vec<(String, Vec<u8>)> {
    let mut parts = pkg.parts.clone();
    let wb = &pkg.workbook;
    let active_tab = wb.active_tab.min(wb.sheets.len().saturating_sub(1));
    // Tab selection is kept in step with the active tab only in a file that
    // marks one (Excel's always do); a file that marks none stays as it is.
    let tabs_selected = pkg.sheet_parts.iter().any(|name| {
        pkg.part(name)
            .is_some_and(|b| tab_is_selected(&String::from_utf8_lossy(b)))
    });

    // --- shared strings: existing entries stay, new text appends ----------
    let mut string_index: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, s) in pkg.shared.iter().enumerate() {
        string_index.entry(s.as_str()).or_insert(i);
    }
    let mut new_list: Vec<String> = Vec::new();
    let mut index_of = |text: &str| -> usize {
        if let Some(&i) = string_index.get(text) {
            return i;
        }
        if let Some(pos) = new_list.iter().position(|s| s == text) {
            return pkg.shared.len() + pos;
        }
        new_list.push(text.to_string());
        pkg.shared.len() + new_list.len() - 1
    };

    // --- dynamic arrays typed here need a `cm` in xl/metadata.xml --------
    let new_cm = if wb
        .sheets
        .iter()
        .any(|sheet| sheet.cells.values().any(needs_new_cm))
    {
        ensure_dynamic_cm(&mut parts)
    } else {
        None
    };

    // --- colour filters' <dxf>s: an equal one the styles part has, or new
    let styles_src = pkg
        .part("xl/styles.xml")
        .map(|b| String::from_utf8_lossy(b).into_owned());
    let have_dxfs: Vec<String> = styles_src
        .as_deref()
        .map(|x| dxf_elements(x).into_iter().map(str::to_string).collect())
        .unwrap_or_default();
    let mut new_dxfs: Vec<String> = Vec::new();
    let mut dxf_for = |cell: bool, rgb: Option<(u8, u8, u8)>| -> Option<u32> {
        styles_src.as_ref()?;
        let x = crate::filter::color_dxf_xml(cell, rgb);
        if let Some(i) = have_dxfs.iter().position(|d| *d == x) {
            return Some(i as u32);
        }
        let k = match new_dxfs.iter().position(|d| *d == x) {
            Some(k) => k,
            None => {
                new_dxfs.push(x);
                new_dxfs.len() - 1
            }
        };
        Some((have_dxfs.len() + k) as u32)
    };

    // --- regenerate each worksheet's sheetData (and cols/dimension) -------
    let mut any_formulas = false;
    for (idx, sheet) in wb.sheets.iter().enumerate() {
        let Some(part_name) = pkg.sheet_parts.get(idx) else {
            continue;
        };
        let source = pkg
            .part(part_name)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        let sheet_data = sheet_data_xml(sheet, &mut index_of, &mut any_formulas, new_cm.as_deref());
        let updated = splice_worksheet(&source, sheet, &sheet_data, &wb.styles.dxfs, &mut dxf_for);
        let updated = if tabs_selected {
            set_tab_selected(&updated, idx == active_tab)
        } else {
            updated
        };
        if let Some(p) = parts.iter_mut().find(|(n, _)| n == part_name) {
            p.1 = updated.into_bytes();
        }
        // Drawings round-trip as their original part, so a moved anchor has to
        // be written back into it.
        // An edited chart's part is regenerated from the model; untouched ones
        // round-trip verbatim, keeping whatever formatting we don't model.
        for dw in &sheet.drawings {
            if let crate::sheet::DrawingKind::Chart(cd) = &dw.kind {
                if let (true, true, Some(cpart)) =
                    (cd.edited, chart_is_writable(cd), cd.part.as_deref())
                {
                    if let Some(p) = parts.iter_mut().find(|(n, _)| n == cpart) {
                        p.1 = chart_space_xml_in(cd, pkg.ns()).into_bytes();
                    }
                }
            }
        }
        if let Some(dpart) = sheet.drawing_part.as_deref() {
            // Every drawing here has a real index in this part, including one
            // `add_chart` spliced in, so moving or deleting any of them
            // addresses the element it actually wrote.
            let moves: Vec<crate::drawing::AnchorMove> = sheet
                .drawings
                .iter()
                .map(|d| (d.anchor_ix, d.from, d.to))
                .collect();
            // Nothing to write means the part is left EXACTLY as it came in.
            // Decoding it first would be lossy for a drawing part that isn't
            // UTF-8 (UTF-16 is legal XML): every stray sequence would come back
            // as U+FFFD, so merely opening and saving would destroy artwork we
            // never touched.
            if !(moves.is_empty() && sheet.drawings_removed.is_empty()) {
                if let Some(p) = parts.iter_mut().find(|(n, _)| n == dpart) {
                    // Lossy on purpose once there IS something to write: the
                    // load path read this same part with `from_utf8_lossy`, so
                    // the anchors and their indices came from the decoded text
                    // either way. Bailing out here instead would drop the move
                    // or the delete on the floor, silently — the chart the user
                    // deleted would be back on the next open.
                    let xml = String::from_utf8_lossy(&p.1);
                    p.1 = crate::drawing::rewrite_anchors(&xml, &moves, &sheet.drawings_removed)
                        .into_bytes();
                }
            }
        }
    }

    // --- authored cell styles: append new xfs to styles.xml ---------------
    if let Some(orig) = pkg.part("xl/styles.xml") {
        let orig = String::from_utf8_lossy(orig).into_owned();
        let base = parse_styles(&orig).xfs.len();
        if wb.styles.xfs.len() > base {
            let updated = splice_styles(&orig, &wb.styles.xfs[base..]);
            if let Some(p) = parts.iter_mut().find(|(n, _)| n == "xl/styles.xml") {
                p.1 = updated.into_bytes();
            }
        }
    }
    if !new_dxfs.is_empty() {
        if let Some(p) = parts.iter_mut().find(|(n, _)| n == "xl/styles.xml") {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            p.1 = append_dxfs(&xml, &new_dxfs).into_bytes();
        }
    }

    // --- shared strings part ----------------------------------------------
    let total = pkg.shared.len() + new_list.len();
    // Append to the workbook's own shared-strings part wherever it lives; only
    // fall back to the conventional path when the workbook has none yet.
    let sst_name = pkg.shared_part.as_deref().unwrap_or("xl/sharedStrings.xml");
    if !new_list.is_empty() || (total > 0 && pkg.part(sst_name).is_none()) {
        let mut additions = String::new();
        for s in &new_list {
            let space = if s.starts_with(char::is_whitespace) || s.ends_with(char::is_whitespace) {
                " xml:space=\"preserve\""
            } else {
                ""
            };
            additions.push_str(&format!("<si><t{space}>{}</t></si>", esc_text(s)));
        }
        match pkg.part(sst_name) {
            Some(orig) => {
                let xml = String::from_utf8_lossy(orig).into_owned();
                let mut updated = xml.replacen("</sst>", &format!("{additions}</sst>"), 1);
                // Self-closing <sst/> (empty table) → expand.
                if updated == xml {
                    if let Some(i) = updated.find("/>") {
                        updated = format!("{}>{additions}</sst>", &updated[..i]);
                    }
                }
                let updated = patch_counts(&updated, total);
                if let Some(p) = parts.iter_mut().find(|(n, _)| n == sst_name) {
                    p.1 = updated.into_bytes();
                }
            }
            None => {
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<sst xmlns=\"{}\" count=\"{total}\" uniqueCount=\"{total}\">{additions}</sst>",
                    pkg.ns().sml
                );
                parts.push((sst_name.to_string(), xml.into_bytes()));
                add_content_type_override(
                    &mut parts,
                    "/xl/sharedStrings.xml",
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml",
                );
                add_workbook_rel(
                    &mut parts,
                    &pkg.ns().rel("sharedStrings"),
                    "sharedStrings.xml",
                );
            }
        }
    }

    // --- tables: geometry, names, columns and conversions reach the parts --
    sync_table_parts(&mut parts, wb);

    // --- pivots: patch the refreshed location, ask Excel to rebuild --------
    // Refresh may have grown/shrunk the output region; the location ref must
    // match what we wrote. refreshOnLoad makes real Excel re-derive its own
    // layout from the same definition on open.
    for piv in &wb.pivots {
        if let Some(p) = parts.iter_mut().find(|(n, _)| n == &piv.part) {
            let mut xml = String::from_utf8_lossy(&p.1).into_owned();
            // An edited field layout rewrites the definition wholesale.
            if piv.edited {
                xml = crate::pivot::rewrite_pivot_definition(&xml, piv);
            }
            let (r1, c1, r2, c2) = piv.location;
            let full = format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2));
            p.1 = patch_ref_attr(&xml, "<location", &full).into_bytes();
        }
        if let Some(p) = parts.iter_mut().find(|(n, _)| n == &piv.cache_part) {
            let mut xml = String::from_utf8_lossy(&p.1).into_owned();
            // Keep the cache's `<worksheetSource sheet="…">` in sync with the
            // model — e.g. after `rename_sheet` rewrites `piv.source`. Renaming
            // doesn't set `piv.edited` (that flag is for field-layout changes,
            // and would force a wholesale table-definition rewrite it doesn't
            // need), so this is the only place the persisted source-sheet name
            // gets corrected.
            match &piv.source {
                crate::pivot::PivotSource::Range { sheet, .. } => {
                    xml = patch_worksheet_source_sheet(&xml, sheet);
                }
                // A renamed source table: the cache names it.
                crate::pivot::PivotSource::Table(name) => {
                    xml = patch_worksheet_source_name(&xml, name);
                }
            }
            p.1 = set_refresh_on_load(&xml).into_bytes();
        }
    }

    // --- sheet names: the model is authoritative ----------------------------
    // workbook.xml is otherwise preserved verbatim, so a rename in the model
    // must be patched into the <sheet name="…"> attributes (in order).
    if let Some(p) = parts.iter_mut().find(|(n, _)| n == "xl/workbook.xml") {
        let xml = String::from_utf8_lossy(&p.1).into_owned();
        let xml = patch_sheet_names(&xml, &wb.sheets);
        let xml = set_active_tab(&xml, active_tab);
        // Same for defined names: a structural edit or a rename moves them in
        // the model (print area and titles included).
        let filtered: Vec<bool> = wb
            .sheets
            .iter()
            .map(|s| s.auto_filter.is_some() || s.filter_mode == Some(true))
            .collect();
        let xml = patch_defined_names(&xml, &wb.defined_names, wb.sheets.len(), &filtered);
        // The date system too: an imported 1904 workbook (#603) starts from
        // new_xlsx's 1900 part, and its dates would shift by 1462 days.
        p.1 = set_date1904(&xml, wb.date1904).into_bytes();
    }

    // --- calc chain: drop it, ask Excel to recalculate ---------------------
    if any_formulas {
        parts.retain(|(n, _)| n != "xl/calcChain.xml");
        if let Some(p) = parts.iter_mut().find(|(n, _)| n == "[Content_Types].xml") {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            p.1 = remove_element_containing(&xml, "<Override", "/xl/calcChain.xml").into_bytes();
        }
        if let Some(p) = parts
            .iter_mut()
            .find(|(n, _)| n == "xl/_rels/workbook.xml.rels")
        {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            p.1 = remove_element_containing(&xml, "<Relationship", "calcChain.xml").into_bytes();
        }
        if let Some(p) = parts.iter_mut().find(|(n, _)| n == "xl/workbook.xml") {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            p.1 = ensure_full_calc(&xml).into_bytes();
        }
    }

    // --- docProps/app.xml: the sheet list follows the model ---------------
    crate::docprops::refresh_titles_of_parts(&mut parts, pkg);

    parts
}

/// Raw leftover attributes ready to follow another attribute: the loader
/// keeps a leading space, but strings built in code may lack one, and
/// `r="1"hidden="1"` is not well-formed.
fn attr_tail(attrs: &str) -> std::borrow::Cow<'_, str> {
    if attrs.is_empty() || attrs.starts_with(char::is_whitespace) {
        attrs.into()
    } else {
        format!(" {attrs}").into()
    }
}

/// `<sheetData>` for one sheet: rows in order, preserved row attrs, cells
/// with values/formulas/styles.
fn sheet_data_xml(
    sheet: &Sheet,
    index_of: &mut impl FnMut(&str) -> usize,
    any_formulas: &mut bool,
    new_cm: Option<&str>,
) -> String {
    let mut out = String::from("<sheetData>");
    // Union of rows that have cells or preserved attributes.
    let mut rows: Vec<u32> = sheet.cells.keys().map(|&(r, _)| r).collect();
    rows.extend(sheet.row_attrs.keys().copied());
    rows.sort_unstable();
    rows.dedup();

    for &row in &rows {
        let attrs = attr_tail(sheet.row_attrs.get(&row).map(|s| s.as_str()).unwrap_or(""));
        let cells: Vec<(&(u32, u32), &Cell)> =
            sheet.cells.range((row, 0)..=(row, u32::MAX)).collect();
        if cells.is_empty() {
            out.push_str(&format!("<row r=\"{}\"{attrs}/>", row + 1));
            continue;
        }
        out.push_str(&format!("<row r=\"{}\"{attrs}>", row + 1));
        for (&(r, c), cell) in cells {
            let taken = block_taken(sheet, r, c, cell);
            out.push_str(&cell_xml(r, c, cell, index_of, any_formulas, new_cm, taken));
        }
        out.push_str("</row>");
    }
    out.push_str("</sheetData>");
    out
}

/// Does another cell hold content inside the block a non-spilling array
/// anchor at `(row, col)` stores in its `ref`? Then the anchor covers its own
/// cell alone ([`cell_xml`]), or the saved block would overlap their content,
/// which Excel never writes. An evaluated legacy CSE block refuses plain
/// edits to part of it, so this is a frozen block content was typed into
/// (#837/#840), a block a formula in it blocks, or content loaded that way.
/// A styled blank isn't content.
fn block_taken(sheet: &Sheet, row: u32, col: u32, cell: &Cell) -> bool {
    if cell.spill.is_some() {
        return false;
    }
    crate::sheet::array_block(cell).is_some_and(|(r1, c1, r2, c2)| {
        (r1, c1) == (row, col)
            && (r1..=r2).any(|r| {
                sheet
                    .cells
                    .range((r, c1)..=(r, c2))
                    .any(|(&k, other)| k != (row, col) && !other.is_blank())
            })
    })
}

/// A dynamic array typed here ([`CellMeta::dynamic`]) that the file has no
/// `cm` for yet, and that is written as an array `<f>`: save gives it the
/// `cm` [`ensure_dynamic_cm`] resolves. One predicate for both, so the part
/// is never extended for a `cm` no cell then carries.
fn needs_new_cm(cell: &Cell) -> bool {
    cell.meta
        .as_ref()
        .is_some_and(|m| m.dynamic && m.cm.is_none())
        && cell.formula.as_deref().is_some_and(|f| !f.is_empty())
        && cell.f_attrs.as_deref().is_none_or(is_array_f)
}

const SHEET_METADATA_CT: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheetMetadata+xml";
const SHEET_METADATA_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/sheetMetadata";
const DYNAMIC_ARRAY_NS: &str =
    "http://schemas.microsoft.com/office/spreadsheetml/2017/dynamicarray";
/// Excel's `XLDAPR` metadata type, as it writes it.
const XLDAPR_TYPE: &str = r#"<metadataType name="XLDAPR" minSupportedVersion="120000" copy="1" pasteAll="1" pasteValues="1" merge="1" splitFirst="1" rowColShift="1" clearFormats="1" clearComments="1" assign="1" coerce="1" cellMeta="1"/>"#;

/// An `XLDAPR` futureMetadata `bk` saying "dynamic array"; `ns` declares the
/// `xda` prefix where the root does not.
fn xldapr_dynamic_bk(ns: &str) -> String {
    format!(
        r#"<bk><extLst><ext uri="{{bdbb8cdc-fa1e-496e-a857-3c3f30c029c3}}"{ns}><xda:dynamicArrayProperties fDynamic="1" fCollapsed="0"/></ext></extLst></bk>"#
    )
}

/// The `cm` index (1-based `cellMetadata` `bk`) that marks a dynamic array in
/// this package, adding what is missing: the metadata part beside the
/// workbook when there is none, otherwise only the absent entries, appended
/// so every index already in use keeps its meaning; and either way the
/// workbook's sheetMetadata relationship and the content-type override. None
/// when the existing part cannot be extended or the workbook cannot be made
/// to reference it: the formula is then written as it was before `cm`
/// existed.
fn ensure_dynamic_cm(parts: &mut Vec<(String, Vec<u8>)>) -> Option<String> {
    let (name, has_rel) = sheet_metadata_part(parts);
    // Work out the edit first: a part we cannot extend changes nothing.
    let edit = match parts.iter().find(|(n, _)| *n == name) {
        Some((_, bytes)) => Some(add_dynamic_cell_metadata(&String::from_utf8_lossy(bytes))?),
        None => None,
    };
    // A `cm` means nothing unless the workbook reaches the part: make sure
    // its relationship exists (in the workbook's own rels part), or give up.
    if !has_rel {
        let wb_part = workbook_part_name(parts);
        let wb_dir = wb_part.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let target = relative_target(wb_dir, &name);
        add_rel(
            parts,
            &rels_part_name(&wb_part),
            SHEET_METADATA_REL,
            &target,
        );
        if sheet_metadata_part(parts) != (name.clone(), true) {
            return None;
        }
    }
    add_content_type_override(parts, &format!("/{name}"), SHEET_METADATA_CT);
    if let Some((updated, index)) = edit {
        if let (Some(updated), Some(p)) = (updated, parts.iter_mut().find(|(n, _)| *n == name)) {
            p.1 = updated.into_bytes();
        }
        return Some(index.to_string());
    }
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<metadata xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xda="{DYNAMIC_ARRAY_NS}"><metadataTypes count="1">{XLDAPR_TYPE}</metadataTypes><futureMetadata name="XLDAPR" count="1">{}</futureMetadata><cellMetadata count="1"><bk><rc t="1" v="0"/></bk></cellMetadata></metadata>"#,
        xldapr_dynamic_bk("")
    );
    parts.push((name, xml.into_bytes()));
    Some("1".to_string())
}

/// Find — or append — the `cellMetadata` entry marking a dynamic array in an
/// existing metadata part: `<rc t="T" v="V"/>` alone in its `bk`, where T is
/// the 1-based `XLDAPR` metadataType and V the 0-based `bk` of the `XLDAPR`
/// futureMetadata saying `fDynamic="1"`. Returns the rewritten part (None
/// when everything was already there) and the entry's 1-based index; None
/// when the part is not one we can edit safely.
fn add_dynamic_cell_metadata(xml: &str) -> Option<(Option<String>, usize)> {
    #[derive(PartialEq)]
    enum In {
        Other,
        Types,
        Dapr,
        Cells,
    }
    let mut p = XmlParser::new(xml);
    let mut depth = 0usize;
    let (mut root_open, mut root_close) = (None, None);
    // The root child being read: what it is, where it starts, its name.
    let mut child = (In::Other, 0usize, "");
    let mut types: Vec<String> = Vec::new();
    let mut types_span = None;
    // End of the last element a new futureMetadata may follow.
    let mut future_anchor = None;
    let mut dapr: Option<(usize, usize)> = None;
    let mut dapr_bks: Vec<bool> = Vec::new();
    let mut cells: Option<(usize, usize)> = None;
    let mut cell_bks: Vec<Vec<(String, String)>> = Vec::new();
    // Start of the first element a new cellMetadata must precede.
    let mut cells_anchor = None;
    loop {
        match p.next() {
            Event::Start => {
                depth += 1;
                let name = local(p.name());
                match depth {
                    1 if p.name() == "metadata" => root_open = Some(p.pos()),
                    1 => return None,
                    2 => {
                        let kind = match name {
                            "metadataTypes" => In::Types,
                            "futureMetadata" if dapr.is_none() && p.attr("name") == "XLDAPR" => {
                                In::Dapr
                            }
                            "cellMetadata" if cells.is_none() => In::Cells,
                            _ => In::Other,
                        };
                        if matches!(name, "valueMetadata" | "extLst") {
                            cells_anchor.get_or_insert(p.start_pos());
                        }
                        child = (kind, p.start_pos(), name);
                    }
                    _ => match (&child.0, name) {
                        (In::Types, "metadataType") if depth == 3 => {
                            types.push(decode(p.attr("name")))
                        }
                        (In::Dapr, "bk") if depth == 3 => dapr_bks.push(false),
                        (In::Dapr, "dynamicArrayProperties") => {
                            if let Some(last) = dapr_bks.last_mut() {
                                *last |= p.attr("fDynamic") == "1" && p.attr("fCollapsed") != "1";
                            }
                        }
                        (In::Cells, "bk") if depth == 3 => cell_bks.push(Vec::new()),
                        (In::Cells, "rc") if depth == 4 => {
                            if let Some(last) = cell_bks.last_mut() {
                                last.push((p.attr("t").to_string(), p.attr("v").to_string()));
                            }
                        }
                        _ => {}
                    },
                }
            }
            Event::End => {
                match depth {
                    1 => root_close = xml[..p.pos()].rfind("</"),
                    2 => {
                        let span = (child.1, p.pos());
                        match child.0 {
                            In::Types => types_span = Some(span),
                            In::Dapr => dapr = Some(span),
                            In::Cells => cells = Some(span),
                            In::Other => {}
                        }
                        if matches!(
                            child.2,
                            "metadataTypes" | "metadataStrings" | "mdxMetadata" | "futureMetadata"
                        ) {
                            future_anchor = Some(span.1);
                        }
                        child = (In::Other, 0, "");
                    }
                    _ => {}
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            Event::Text => {}
        }
    }
    let (root_open, root_close) = (root_open?, root_close?);
    if root_close < root_open {
        return None;
    }

    // (start, end, replacement), applied back to front.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let t = match types.iter().position(|n| n == "XLDAPR") {
        Some(i) => i + 1,
        None => {
            let edit = match types_span {
                Some((a, b)) => (a, b, append_child(&xml[a..b], XLDAPR_TYPE, types.len() + 1)),
                None => (
                    root_open,
                    root_open,
                    format!(r#"<metadataTypes count="1">{XLDAPR_TYPE}</metadataTypes>"#),
                ),
            };
            edits.push(edit);
            types.len() + 1
        }
    };
    let bk = xldapr_dynamic_bk(&format!(r#" xmlns:xda="{DYNAMIC_ARRAY_NS}""#));
    let v = match (dapr_bks.iter().position(|&d| d), dapr) {
        (Some(i), _) => i,
        (None, Some((a, b))) => {
            edits.push((a, b, append_child(&xml[a..b], &bk, dapr_bks.len() + 1)));
            dapr_bks.len()
        }
        (None, None) => {
            let at = future_anchor.unwrap_or(root_open);
            edits.push((
                at,
                at,
                format!(r#"<futureMetadata name="XLDAPR" count="1">{bk}</futureMetadata>"#),
            ));
            0
        }
    };
    let pair = (t.to_string(), v.to_string());
    if edits.is_empty() {
        if let Some(i) = cell_bks.iter().position(|b| b.len() == 1 && b[0] == pair) {
            return Some((None, i + 1));
        }
    }
    let entry = format!(r#"<bk><rc t="{t}" v="{v}"/></bk>"#);
    let index = match cells {
        Some((a, b)) => {
            edits.push((a, b, append_child(&xml[a..b], &entry, cell_bks.len() + 1)));
            cell_bks.len() + 1
        }
        None => {
            let at = cells_anchor.unwrap_or(root_close);
            edits.push((
                at,
                at,
                format!(r#"<cellMetadata count="1">{entry}</cellMetadata>"#),
            ));
            1
        }
    };
    // Back to front. Of two insertions at one spot, the one pushed first
    // (metadataTypes before futureMetadata) ends up first.
    edits.sort_by_key(|e| e.0);
    let mut out = xml.to_string();
    for (a, b, text) in edits.into_iter().rev() {
        out.replace_range(a..b, &text);
    }
    Some((Some(out), index))
}

/// `el` (one whole element) with `child` appended as its last child and its
/// `count` attribute set to `count`.
fn append_child(el: &str, child: &str, count: usize) -> String {
    let with_count = |head: &str| match head.find(" count=\"") {
        Some(i) => {
            let vs = i + " count=\"".len();
            let ve = head[vs..].find('"').map_or(head.len(), |e| vs + e);
            format!("{}{count}{}", &head[..vs], &head[ve..])
        }
        None => format!("{head} count=\"{count}\""),
    };
    if let Some(head) = el.strip_suffix("/>") {
        let name_end = el[1..]
            .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .map_or(el.len(), |e| e + 1);
        return format!(
            "{}>{child}</{}>",
            with_count(head.trim_end()),
            &el[1..name_end]
        );
    }
    let gt = el.find('>').unwrap_or(0);
    let close = el.rfind("</").unwrap_or(el.len());
    format!(
        "{}{}{child}{}",
        with_count(&el[..gt]),
        &el[gt..close],
        &el[close..]
    )
}

fn cell_xml(
    row: u32,
    col: u32,
    cell: &Cell,
    index_of: &mut impl FnMut(&str) -> usize,
    any_formulas: &mut bool,
    new_cm: Option<&str>,
    block_taken: bool,
) -> String {
    let mut attrs = format!(" r=\"{}\"", cell_name(row, col));
    if cell.style != 0 {
        attrs.push_str(&format!(" s=\"{}\"", cell.style));
    }
    let has_formula = cell.formula.is_some();
    if has_formula {
        *any_formulas = true;
    }

    // Type attribute + value body depend on the value kind. Formula cells
    // carry their cached value with t="str" for text; plain text cells go
    // through the shared-string table.
    // A decoded rich error keeps the body the file had while its `vm` does.
    let vm_body = cell
        .meta
        .as_deref()
        .and_then(|m| match (&m.vm, &m.vm_body) {
            (Some((_, snap)), Some(body)) if *snap == cell.value => Some(body.as_str()),
            _ => None,
        });
    let (t_attr, body) = match &cell.value {
        CellValue::Error(_) if vm_body.is_some() => (
            " t=\"e\"",
            format!("<v>{}</v>", esc_text(vm_body.unwrap_or_default())),
        ),
        CellValue::Empty => ("", String::new()),
        CellValue::Number(n) => ("", format!("<v>{}</v>", num_repr(*n))),
        CellValue::Bool(b) => (" t=\"b\"", format!("<v>{}</v>", u8::from(*b))),
        CellValue::Error(e) => (" t=\"e\"", format!("<v>{}</v>", esc_text(e))),
        CellValue::Text(s) => {
            if has_formula {
                (" t=\"str\"", format!("<v>{}</v>", esc_text(s)))
            } else {
                (" t=\"s\"", format!("<v>{}</v>", index_of(s)))
            }
        }
    };

    // A dynamic array typed here gets the `cm` save resolved for it; without
    // one (a metadata part we could not extend) it is written as before.
    let new_cm = new_cm.filter(|_| needs_new_cm(cell));
    let dynamic = cell.has_cm() || new_cm.is_some();
    let anchor = cell_name(row, col);
    let (f_xml, array_f) = match (&cell.formula, &cell.f_attrs) {
        // A spilling anchor writes fresh array attributes — its extent may
        // have changed since load, so any stored ref would be stale. A 1x1
        // extent is no spill beyond the anchor: it falls to the arms below,
        // which write the anchor-only ref (or a plain `<f>`) as before.
        (Some(src), _) if cell.spill.is_some_and(|ext| ext != (1, 1)) && !src.is_empty() => {
            let (h, w) = cell.spill.unwrap();
            let f = format!(
                "<f t=\"array\" ref=\"{}:{}\">{}</f>",
                cell_name(row, col),
                cell_name(row + h - 1, col + w - 1),
                esc_text(&file_formula(src))
            );
            (f, true)
        }
        // A dynamic array that does not spill now (a 1x1 result, or
        // #SPILL!) after an edit went through `Engine::set_cell`, which drops
        // `f_attrs`, or typed here: its `cm` says it is still one, covering
        // its anchor alone.
        (Some(src), None) if dynamic && !src.is_empty() => (
            format!(
                "<f t=\"array\" ref=\"{anchor}\">{}</f>",
                esc_text(&file_formula(src))
            ),
            true,
        ),
        (Some(src), None) => (format!("<f>{}</f>", esc_text(&file_formula(src))), false),
        (Some(src), Some(fa)) if src.is_empty() => (format!("<f{fa}/>"), is_array_f(fa)),
        // A non-spilling array covers its anchor alone when its stored ref
        // is stale: a dynamic array's cells were cleared since load, and a
        // ref that starts elsewhere names another block (a cell moved
        // without set_cell, a sort say, or loaded that way; set_cell and
        // paste re-anchor themselves). A legacy CSE block (no `cm`) whose ref
        // starts here keeps it, and Excel refills it on load, unless another
        // cell in it now holds content ([`block_taken`]).
        (Some(src), Some(fa))
            if is_array_f(fa) && (dynamic || block_taken || !ref_starts_at(fa, &anchor)) =>
        {
            (
                format!(
                    "<f{}>{}</f>",
                    with_ref(fa, &anchor),
                    esc_text(&file_formula(src))
                ),
                true,
            )
        }
        (Some(src), Some(fa)) => (
            format!("<f{fa}>{}</f>", esc_text(&file_formula(src))),
            is_array_f(fa),
        ),
        (None, _) => (String::new(), false),
    };

    // Schema order after `t`: cm, vm, ph. `cm` marks a dynamic array, so it
    // only belongs on an array `<f>`; `vm` describes the loaded value.
    let mut tail = String::new();
    if let Some(m) = &cell.meta {
        if let Some(cm) = m.cm.as_deref().or(new_cm).filter(|_| array_f) {
            tail.push_str(&format!(" cm=\"{}\"", esc_raw_attr(cm)));
        }
        if let Some((vm, _)) = m.vm.as_ref().filter(|(_, v)| *v == cell.value) {
            tail.push_str(&format!(" vm=\"{}\"", esc_raw_attr(vm)));
        }
        if m.ph {
            tail.push_str(" ph=\"1\"");
        }
    }

    if body.is_empty() && f_xml.is_empty() {
        format!("<c{attrs}{tail}/>")
    } else {
        format!("<c{attrs}{t_attr}{tail}>{f_xml}{body}</c>")
    }
}

/// Replace `<sheetData>…</sheetData>` (or `<sheetData/>`) in the original
/// worksheet XML, refresh `<dimension>`, and regenerate `<cols>`.
///
/// The cells always land when the part has a readable `<sheetData>`, and so
/// do merges and protection ([`sync_worksheet_child`]). `<cols>` and a new
/// `<sheetViews>` follow [`put_worksheet_child`]: on a malformed part with no
/// known position for them they are left as they were.
fn splice_worksheet(
    source: &str,
    sheet: &Sheet,
    sheet_data: &str,
    dxfs: &[crate::sheet::Dxf],
    dxf_for: &mut DxfFor,
) -> String {
    // Found by local name, so `<x:sheetData>` is replaced, not joined by a
    // second one. A worksheet that has none gets ours at its schema position.
    let walk_found = worksheet_child_span(source, "sheetData").is_some();
    let stopped = worksheet_children(source).close.is_none();
    let mut out = if walk_found || !stopped {
        put_worksheet_child(source, "sheetData", sheet_data, None, true)
    } else {
        match sheet_data_fallback_span(source) {
            // The walk stopped before sheetData (a malformed child ahead of
            // it): find it by its tags, so the edits are saved rather than
            // silently dropped.
            Some((s, e)) => {
                let block = match worksheet_root(source) {
                    Some(root) => in_worksheet_ns(&root, sheet_data),
                    None => sheet_data.to_string(),
                };
                format!("{}{block}{}", &source[..s], &source[e..])
            }
            None => put_worksheet_child(source, "sheetData", sheet_data, None, true),
        }
    };

    // <dimension ref="…"/> → recomputed used range.
    let (rows, cols) = sheet.used_size();
    let dim = if rows == 0 {
        "A1".to_string()
    } else {
        format!("A1:{}", cell_name(rows - 1, cols.max(1) - 1))
    };
    if let Some((i, _)) = worksheet_child_span(&out, "dimension") {
        if attr_at(&out, i, "ref").is_some() {
            out = set_tag_attr(&out, i, "ref", Some(&dim));
        }
    }

    // <cols> — regenerate from the model when we have definitions, and drop
    // the element when the last one went (an ungrouped column's).
    if sheet.col_defs.is_empty() {
        if worksheet_child_span(&out, "cols").is_some() {
            out = remove_worksheet_child(&out, "cols");
        }
    } else {
        let mut cols_xml = String::from("<cols>");
        for d in &sheet.col_defs {
            // Excel reads a `<col>` with no width as zero wide: one created
            // for an outline or a hide gets the sheet's default. One loaded
            // without a width keeps its spelling.
            let width = match d.width {
                Some(w) => format!(" width=\"{w}\" customWidth=\"1\""),
                None if d.default_width => {
                    format!(" width=\"{}\"", sheet.default_col_file_width())
                }
                None => String::new(),
            };
            cols_xml.push_str(&format!(
                "<col min=\"{}\" max=\"{}\"{width}{}/>",
                d.min + 1,
                d.max + 1,
                attr_tail(&d.attrs)
            ));
        }
        cols_xml.push_str("</cols>");
        out = put_worksheet_child(&out, "cols", &cols_xml, None, true);
    }

    // Frozen panes: sync the first sheetView's <pane> from the model.
    let out = set_freeze_pane(&out, sheet.freeze);
    // Merged regions: regenerate <mergeCells> from the model.
    let out = set_merge_cells(&out, &sheet.merges);
    // Sheet protection: <sheetProtection> regenerated at its CT_Worksheet
    // position.
    let out = set_sheet_protection(&out, sheet.protection.as_deref());
    // Page breaks (manual and automatic): rewritten only where a structural
    // edit moved them.
    let out = set_page_breaks(out, "rowBreaks", &sheet.row_breaks);
    let out = set_page_breaks(out, "colBreaks", &sheet.col_breaks);
    // Page setup: only the attributes that changed since the load.
    let out = page::set_page_setup(&out, &sheet.page_setup, &sheet.page_setup_loaded);
    // Outline: `outlinePr` only when its settings changed; the level
    // summary in `sheetFormatPr` from the live rows and columns.
    let out = page::set_outline_pr(&out, sheet.outline, sheet.outline_loaded);
    let out = page::set_outline_levels(
        &out,
        sheet.max_row_outline(),
        sheet.max_col_outline(),
        sheet.format.default_row_height,
    );
    // Data > Consolidate's settings: only when they changed.
    let out = consolidate::write(
        &out,
        sheet.consolidate.as_ref(),
        sheet.consolidate_loaded.as_ref(),
    );
    // The sheet's autoFilter: kept while it matches the model, else moved,
    // rewritten from the model, added or dropped (see set_auto_filter).
    let out = set_auto_filter(out, sheet.auto_filter.as_ref(), dxfs, dxf_for);
    let out = set_filter_mode(out, sheet.filter_mode);
    // Conditional formatting and data validation: likewise.
    let out = set_cond_formats(out, sheet);
    let out = set_validations(out, sheet);
    // Hyperlinks: a removed link's element goes.
    set_hyperlinks(out, sheet)
}

/// Strike the `<hyperlink>` elements whose links were removed
/// ([`Sheet::hyperlinks_removed`]), and the `<hyperlinks>` block once it is
/// empty. The external relationship a removed link used stays in the rels,
/// unreferenced, which OPC allows. Every other element is left as it is.
fn set_hyperlinks(xml: String, sheet: &Sheet) -> String {
    if sheet.hyperlinks_removed.is_empty() {
        return xml;
    }
    let Some((ws, we)) = worksheet_child_span(&xml, "hyperlinks") else {
        return xml;
    };
    let block = &xml[ws..we];
    let items: Vec<(usize, usize)> = element_children(block)
        .into_iter()
        .filter(|(name, _, _)| name == "hyperlink")
        .map(|(_, a, b)| (a, b))
        .collect();
    // A set, and one pass over the block: a sheet of 50k links stays
    // linear (#707 r6 M2).
    let removed: std::collections::HashSet<(u32, u32, u32, u32)> =
        sheet.hyperlinks_removed.iter().copied().collect();
    let mut drop = Vec::new();
    for &(a, b) in &items {
        let Some(tag) = start_tag(&block[a..b]) else {
            continue;
        };
        let Some(&(_, vs, ve)) = tag.attrs.iter().find(|(n, _, _)| *n == "ref") else {
            continue;
        };
        let r = &block[a + vs..a + ve];
        let rect = crate::sheet::parse_range_name(r)
            .or_else(|| crate::sheet::parse_cell_name(r).map(|(r, c)| (r, c, r, c)));
        if rect.is_some_and(|rect| removed.contains(&rect)) {
            drop.push((a, b));
        }
    }
    if drop.is_empty() {
        return xml;
    }
    if drop.len() == items.len() {
        return remove_worksheet_child(&xml, "hyperlinks");
    }
    let mut kept = String::with_capacity(block.len());
    let mut from = 0;
    for (a, b) in drop {
        kept.push_str(&block[from..a]);
        from = b;
    }
    kept.push_str(&block[from..]);
    format!("{}{kept}{}", &xml[..ws], &xml[we..])
}

/// The worksheet's top-level `<conditionalFormatting>` elements (in any
/// prefix; never an x14 one in `extLst`), in document order, as (start, end).
/// A block's [`crate::sheet::CondFormat::ix`] is its position here, for the loader and the
/// save alike.
fn cond_format_spans(xml: &str) -> Vec<(usize, usize)> {
    // Most sheets have none: don't walk the part to learn that.
    if !xml.contains("conditionalFormatting") {
        return Vec::new();
    }
    worksheet_children(xml)
        .children
        .iter()
        .filter(|c| c.local == "conditionalFormatting")
        .map(|c| (c.start, c.end))
        .collect()
}

/// An XML boolean attribute's value (`1` / `true`).
fn flag_attr(v: &str) -> bool {
    matches!(v, "1" | "true")
}

/// A (start, end) byte span in a part.
type Span = (usize, usize);

/// The worksheet's top-level `<dataValidations>` and its `<dataValidation>`
/// children, as (start, end) in `xml`. A rule's
/// [`crate::sheet::DataValidation::ix`] is its position among the children.
fn validation_spans(xml: &str) -> Option<(Span, Vec<Span>)> {
    if !xml.contains("dataValidations") {
        return None;
    }
    let (s, e) = worksheet_child_span(xml, "dataValidations")?;
    let items = element_children(&xml[s..e])
        .into_iter()
        .filter(|(name, _, _)| name == "dataValidation")
        .map(|(_, a, b)| (s + a, s + b))
        .collect();
    Some(((s, e), items))
}

/// The ranges of an element's `sqref`, read as the loader reads them; `None`
/// when a token doesn't read as a cell or range (a whole column, say), since
/// writing the model's ranges back would lose it.
fn held_sqref(element: &str) -> Option<Vec<(u32, u32, u32, u32)>> {
    let tag = start_tag(element)?;
    let &(_, vs, ve) = tag.attrs.iter().find(|(name, _, _)| *name == "sqref")?;
    element[vs..ve]
        .split_whitespace()
        .map(crate::sheet::parse_range_name)
        .collect()
}

/// The start tag a fragment begins with, read with quotes respected: a
/// `>` inside an attribute value (`error="must be > 0"`, legal and written
/// by some producers) doesn't end it, as it would for [`tag_end`].
struct StartTag<'a> {
    /// Where the element name ends.
    name_end: usize,
    /// Each attribute: its name and its value's span.
    attrs: Vec<(&'a str, usize, usize)>,
}

/// [`StartTag`] of `element`; `None` when it doesn't read as one.
fn start_tag(element: &str) -> Option<StartTag<'_>> {
    let b = element.as_bytes();
    let stop = |c: u8| c.is_ascii_whitespace() || matches!(c, b'=' | b'>' | b'/');
    if b.first() != Some(&b'<') {
        return None;
    }
    let mut i = 1;
    while b.get(i).is_some_and(|&c| !stop(c)) {
        i += 1;
    }
    let mut tag = StartTag {
        name_end: i,
        attrs: Vec::new(),
    };
    loop {
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        match *b.get(i)? {
            b'>' => return Some(tag),
            b'/' => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let name_start = i;
        while b.get(i).is_some_and(|&c| !stop(c)) {
            i += 1;
        }
        let name = &element[name_start..i];
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if b.get(i) != Some(&b'=') {
            if name.is_empty() {
                return None;
            }
            continue;
        }
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let q = *b.get(i).filter(|c| matches!(c, b'"' | b'\''))?;
        let start = i + 1;
        let end = start + element[start..].find(q as char)?;
        tag.attrs.push((name, start, end));
        i = end + 1;
    }
}

/// Where the start tag `element` begins with ends (past its `>`), quotes
/// respected; `None` when it doesn't read as one.
fn start_tag_end(element: &str) -> Option<usize> {
    let tag = start_tag(element)?;
    let from = tag.attrs.last().map_or(tag.name_end, |&(_, _, e)| e + 1);
    element[from..].find('>').map(|i| from + i + 1)
}

/// `ranges` as an `sqref` value.
fn sqref_of(ranges: &[(u32, u32, u32, u32)]) -> String {
    ranges
        .iter()
        .map(|&(r1, c1, r2, c2)| {
            if (r1, c1) == (r2, c2) {
                cell_name(r1, c1)
            } else {
                format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A formula element as a part holds it: its decoded text, and the span of
/// its content within the fragment it was read from (`None` when it is
/// self-closing or holds markup, so it can't be rewritten in place).
struct HeldFormula {
    text: String,
    content: Option<(usize, usize)>,
}

/// The direct children of `element` (a fragment that starts with its start
/// tag) named `name`, read as formulas, with spans relative to `element`.
fn held_formulas(element: &str, name: &str) -> Vec<HeldFormula> {
    element_children(element)
        .into_iter()
        .filter(|(n, _, _)| n == name)
        .map(|(_, s, e)| {
            let f = &element[s..e];
            let open = start_tag_end(f).unwrap_or(f.len());
            let close = f.rfind("</").filter(|&c| c >= open && !f.ends_with("/>"));
            match close {
                Some(c) if !f[open..c].contains('<') => HeldFormula {
                    text: decode(&f[open..c]),
                    content: Some((s + open, s + c)),
                },
                _ => HeldFormula {
                    text: String::new(),
                    content: None,
                },
            }
        })
        .collect()
}

/// Do a held formula and the model's say the same thing, spelling aside?
/// Both are compared as parsed when both parse (`_xlfn.XOR` is `XOR`).
fn same_formula(held: &str, model: &str) -> bool {
    if held == model {
        return true;
    }
    match (crate::formula::parse(held), crate::formula::parse(model)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// `element` with its start tag's `attr` set to `value` in place, so the
/// attribute order is kept; inserted after the element name when it has
/// none. A start tag that doesn't read ([`start_tag`]) leaves `element`
/// unchanged.
fn set_tag_attr_in_place(mut element: String, attr: &str, value: &str) -> String {
    let Some(tag) = start_tag(&element) else {
        return element;
    };
    match tag.attrs.iter().find(|(name, _, _)| *name == attr) {
        Some(&(_, vs, ve)) => element.replace_range(vs..ve, value),
        None => {
            let at = tag.name_end;
            element.insert_str(at, &format!(" {attr}=\"{value}\""));
        }
    }
    element
}

/// The edits, as (start, end, text), that write each `model` formula whose
/// held counterpart says otherwise into that counterpart's content. Counts
/// that differ leave the formulas as they are.
fn formula_edits(held: &[HeldFormula], model: &[&String]) -> Vec<(usize, usize, String)> {
    if held.len() != model.len() {
        return Vec::new();
    }
    held.iter()
        .zip(model)
        .filter(|(h, m)| !same_formula(&h.text, m))
        .filter_map(|(h, m)| h.content.map(|(s, e)| (s, e, esc_text(&file_formula(m)))))
        .collect()
}

/// `s` with (start, end, text) edits applied, back to front.
fn apply_edits(mut s: String, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|&(start, _, _)| std::cmp::Reverse(start));
    for (start, end, text) in edits {
        s.replace_range(start..end, &text);
    }
    s
}

/// The one model entry whose `ix` names element `k`: `None` when none does,
/// or when more than one does (a stale claim; nothing says which is right).
fn sole_claim<T>(
    items: &[T],
    k: usize,
    ix: impl Fn(&T) -> Option<usize>,
) -> Result<Option<&T>, ()> {
    let mut claims = items.iter().filter(|t| ix(t) == Some(k));
    match (claims.next(), claims.next()) {
        (Some(_), Some(_)) => Err(()),
        (one, _) => Ok(one),
    }
}

/// Sync the worksheet's `<conditionalFormatting>` elements with the model,
/// as [`set_auto_filter`] does the filter: an element is matched to the
/// model block whose [`crate::sheet::CondFormat::ix`] names it, and left byte-for-byte
/// alone while it holds that block's ranges and formulas. A moved block gets
/// a new `sqref` and its changed `<formula>` texts, everything else kept; a
/// block an edit deleted ([`Sheet::cf_removed`]: a row or column delete,
/// Clear All or Clear Formats) loses its element.
/// An element two model blocks claim, or whose `sqref` doesn't read, is left
/// as it is.
fn set_cond_formats(xml: String, sheet: &Sheet) -> String {
    if sheet.cond_formats.iter().all(|cf| cf.ix.is_none()) && sheet.cf_removed.is_empty() {
        return xml;
    }
    let mut edits = Vec::new();
    for (k, &(start, end)) in cond_format_spans(&xml).iter().enumerate() {
        let element = &xml[start..end];
        let Some(held) = held_sqref(element) else {
            continue;
        };
        let cf = match sole_claim(&sheet.cond_formats, k, |cf| cf.ix) {
            Err(()) => continue,
            Ok(None) => {
                if sheet.cf_removed.contains(&k) {
                    edits.push((start, end, String::new()));
                }
                continue;
            }
            Ok(Some(cf)) => cf,
        };
        let rules: Vec<(usize, usize)> = element_children(element)
            .into_iter()
            .filter(|(n, _, _)| n == "cfRule")
            .map(|(_, s, e)| (s, e))
            .collect();
        if rules.len() != cf.rules.len() {
            continue;
        }
        let mut inner = Vec::new();
        for (&(rs, re), rule) in rules.iter().zip(&cf.rules) {
            let held_f = held_formulas(&element[rs..re], "formula");
            inner.extend(
                formula_edits(&held_f, &rule.formulas())
                    .into_iter()
                    .map(|(s, e, t)| (rs + s, rs + e, t)),
            );
        }
        if held == cf.ranges && inner.is_empty() {
            continue;
        }
        let mut block = apply_edits(element.to_string(), inner);
        if held != cf.ranges {
            block = set_tag_attr_in_place(block, "sqref", &sqref_of(&cf.ranges));
        }
        edits.push((start, end, block));
    }
    apply_edits(xml, edits)
}

/// Sync the worksheet's `<dataValidation>` elements with the model the way
/// [`set_cond_formats`] does the conditional formatting: `sqref`,
/// `<formula1>` and `<formula2>` follow a moved rule, a deleted one
/// ([`Sheet::dv_removed`]) loses its element, and the `<dataValidations>`
/// around them keeps a right `count`, or goes once it holds none. A rule the
/// dialog edited has the attributes that differ from the part
/// ([`crate::sheet::DataValidation::orig`]) written in place, every other
/// attribute (unknown ones too) kept; a rule built in memory is appended.
fn set_validations(xml: String, sheet: &Sheet) -> String {
    let xml = edit_validations(xml, sheet);
    let new: Vec<&crate::sheet::DataValidation> = sheet
        .validations
        .iter()
        .filter(|dv| dv.ix.is_none() && dv.orig.is_none() && !dv.ranges.is_empty())
        .collect();
    if new.is_empty() {
        return xml;
    }
    // All the new rules in one pass over the part, not one per rule.
    let items: Vec<String> = new.iter().map(|dv| dv_element(dv)).collect();
    match append_all_to_worksheet_child(&xml, "dataValidations", &items) {
        Some(out) => out,
        None if worksheet_takes(&xml, "dataValidations", true) => {
            let block = format!(
                "<dataValidations count=\"{}\">{}</dataValidations>",
                items.len(),
                items.concat()
            );
            put_worksheet_child(&xml, "dataValidations", &block, None, false)
        }
        None => xml,
    }
}

/// A new `<dataValidation>` element for `dv`, attributes at their defaults
/// left out.
fn dv_element(dv: &crate::sheet::DataValidation) -> String {
    let mut attrs = String::new();
    let mut put = |name: &str, value: &str| {
        attrs.push_str(&format!(" {name}=\"{}\"", esc_attr(value)));
    };
    for (name, value) in dv_attrs(dv) {
        if let Some(value) = value {
            put(name, &value);
        }
    }
    put("sqref", &sqref_of(&dv.ranges));
    let mut kids = String::new();
    for (tag, f) in [("formula1", &dv.formula1), ("formula2", &dv.formula2)] {
        if !f.is_empty() {
            kids.push_str(&format!("<{tag}>{}</{tag}>", esc_text(&file_formula(f))));
        }
    }
    format!("<dataValidation{attrs}>{kids}</dataValidation>")
}

/// The attributes of `dv` other than `sqref`, in schema order, each `None`
/// when it is at its default and so absent from the element.
fn dv_attrs(dv: &crate::sheet::DataValidation) -> [(&'static str, Option<String>); 11] {
    use crate::sheet::AlertStyle;
    let text = |s: &str| (!s.is_empty()).then(|| s.to_string());
    let flag = |on: bool| on.then(|| "1".to_string());
    [
        ("type", text(&dv.kind).filter(|k| k != "none")),
        (
            "errorStyle",
            (dv.error_style != AlertStyle::Stop).then(|| dv.error_style.attr().to_string()),
        ),
        ("operator", text(&dv.operator)),
        ("allowBlank", flag(dv.allow_blank)),
        ("showDropDown", flag(!dv.show_dropdown)),
        ("showInputMessage", flag(dv.show_input)),
        ("showErrorMessage", flag(dv.show_error)),
        ("errorTitle", text(&dv.error_title)),
        ("error", text(&dv.error)),
        ("promptTitle", text(&dv.prompt_title)),
        ("prompt", dv.prompt.clone()),
    ]
}

/// `element` with its start tag's `attr` taken out; unchanged when absent.
fn remove_tag_attr(mut element: String, attr: &str) -> String {
    let Some(tag) = start_tag(&element) else {
        return element;
    };
    let Some(&(_, vs, ve)) = tag.attrs.iter().find(|(name, _, _)| *name == attr) else {
        return element;
    };
    let Some(name_at) = element[..vs].rfind(attr) else {
        return element;
    };
    // The whitespace before the name goes with it; the closing quote too.
    let from = element[..name_at].trim_end().len();
    element.replace_range(from..ve + 1, "");
    element
}

/// `element` with the attributes of `dv` that differ from `orig` written.
fn dv_attr_edits(
    mut element: String,
    dv: &crate::sheet::DataValidation,
    orig: &crate::sheet::DataValidation,
) -> String {
    for ((name, new), (_, old)) in dv_attrs(dv).into_iter().zip(dv_attrs(orig)) {
        if new == old {
            continue;
        }
        element = match new {
            // The element's own delimiter is kept, which may be `'`: escape it.
            Some(v) => set_tag_attr_in_place(element, name, &esc_attr(&v).replace('\'', "&apos;")),
            None => remove_tag_attr(element, name),
        };
    }
    element
}

/// The element's formulas rebuilt: every `<formula1>`/`<formula2>` child
/// replaced. For an edited rule when the model has a different number of them
/// than the part (an operator that takes one bound or two, a rule that gained
/// or lost its formula), or when the part holds one as markup (a CDATA
/// section), which can't be rewritten in place.
fn dv_rebuild_formulas(element: &str, dv: &crate::sheet::DataValidation) -> String {
    let Some(open) = start_tag_end(element) else {
        return element.to_string();
    };
    let head = &element[..open];
    let name = head[1..]
        .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .next()
        .unwrap_or("dataValidation");
    let prefix = name.rsplit_once(':').map_or("", |(p, _)| p);
    let tag = |local: &str| {
        if prefix.is_empty() {
            local.to_string()
        } else {
            format!("{prefix}:{local}")
        }
    };
    let head = match head.strip_suffix("/>") {
        Some(h) => format!("{}>", h.trim_end()),
        None => head.to_string(),
    };
    let mut out = head;
    for (local, f) in [("formula1", &dv.formula1), ("formula2", &dv.formula2)] {
        if !f.is_empty() {
            let t = tag(local);
            out.push_str(&format!("<{t}>{}</{t}>", esc_text(&file_formula(f))));
        }
    }
    out.push_str(&format!("</{name}>"));
    out
}

/// [`set_validations`]'s edits to the elements the part already holds.
fn edit_validations(xml: String, sheet: &Sheet) -> String {
    if sheet.validations.iter().all(|dv| dv.ix.is_none()) && sheet.dv_removed.is_empty() {
        return xml;
    }
    let Some(((ws, we), items)) = validation_spans(&xml) else {
        return xml;
    };
    // Edits within the wrapper, relative to its start.
    let mut edits = Vec::new();
    let mut removed = 0;
    for (k, &(start, end)) in items.iter().enumerate() {
        let element = &xml[start..end];
        let Some(held) = held_sqref(element) else {
            continue;
        };
        let dv = match sole_claim(&sheet.validations, k, |dv| dv.ix) {
            Err(()) => continue,
            Ok(None) => {
                if sheet.dv_removed.contains(&k) {
                    edits.push((start - ws, end - ws, String::new()));
                    removed += 1;
                }
                continue;
            }
            Ok(Some(dv)) => dv,
        };
        let f1 = held_formulas(element, "formula1");
        let f2 = held_formulas(element, "formula2");
        let count_differs = f1.len() != usize::from(!dv.formula1.is_empty())
            || f2.len() != usize::from(!dv.formula2.is_empty());
        // Only a rule edited since the load has its formula count rewritten:
        // a part's odd one is left as it is.
        let edited = dv
            .orig
            .as_deref()
            .is_some_and(|o| o.formula1 != dv.formula1 || o.formula2 != dv.formula2);
        // A formula held as markup (a CDATA section) can't be rewritten in
        // place: an edited one has its formulas rebuilt.
        let opaque = f1.iter().chain(&f2).any(|f| f.content.is_none());
        let rebuild = edited && (count_differs || opaque);
        let mut block = if rebuild {
            dv_rebuild_formulas(element, dv)
        } else {
            let mut inner = formula_edits(&f1, &[&dv.formula1]);
            if !(f2.is_empty() && dv.formula2.is_empty()) {
                inner.extend(formula_edits(&f2, &[&dv.formula2]));
            }
            apply_edits(element.to_string(), inner)
        };
        if let Some(orig) = dv.orig.as_deref() {
            block = dv_attr_edits(block, dv, orig);
        }
        if held != dv.ranges {
            block = set_tag_attr_in_place(block, "sqref", &sqref_of(&dv.ranges));
        }
        if block != element {
            edits.push((start - ws, end - ws, block));
        }
    }
    if edits.is_empty() {
        return xml;
    }
    let wrapper = if removed == items.len() {
        String::new()
    } else {
        let wrapper = apply_edits(xml[ws..we].to_string(), edits);
        let has_count = start_tag(&wrapper)
            .is_some_and(|t| t.attrs.iter().any(|(name, _, _)| *name == "count"));
        if removed > 0 && has_count {
            set_tag_attr_in_place(wrapper, "count", &(items.len() - removed).to_string())
        } else {
            wrapper
        }
    };
    apply_edits(xml, vec![(ws, we, wrapper)])
}

/// The span of the sheet's own `<autoFilter>`: a top-level one, not a custom
/// view's.
fn sheet_auto_filter_span(xml: &str) -> Option<(usize, usize)> {
    // Most sheets have no filter: don't walk the part to learn that.
    if !xml.contains("autoFilter") {
        return None;
    }
    worksheet_child_span(xml, "autoFilter")
}

/// The position and criteria an `<autoFilter>` element (the whole element,
/// from its start tag) holds; `None` when its `ref` is not a range.
fn auto_filter_position(
    element: &str,
    dxfs: &[crate::sheet::Dxf],
) -> Option<crate::sheet::SheetAutoFilter> {
    let range = crate::sheet::parse_range_name(attr_at(element, 0, "ref")?)?;
    let columns = element_children(element)
        .into_iter()
        .filter(|(name, _, _)| name == "filterColumn")
        // `colId` defaults to 0 as the criteria reader reads it.
        .map(|(_, s, _)| {
            Some(
                range.1
                    + attr_at(element, s, "colId")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
            )
        })
        .collect();
    let mut criteria: Vec<(u32, crate::filter::ColumnFilter)> =
        crate::filter::filter_columns(element, dxfs)
            .into_iter()
            .map(|(id, f)| (range.1 + id, f))
            .collect();
    criteria.sort_by_key(|(c, _)| *c);
    Some(crate::sheet::SheetAutoFilter {
        range,
        columns,
        criteria,
    })
}

/// The direct children of the element that `xml` starts with, as (local
/// name, start, end), found with the loader's parser.
fn element_children(xml: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(xml);
    let mut depth = 0usize;
    let mut open: Option<(String, usize)> = None;
    loop {
        match p.next() {
            Event::Start => {
                depth += 1;
                if depth == 2 {
                    open = Some((local(p.name()).to_string(), p.start_pos()));
                }
            }
            Event::End => {
                if depth == 2 {
                    if let Some((name, start)) = open.take() {
                        out.push((name, start, p.pos()));
                    }
                }
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            Event::Text => {}
        }
    }
    out
}

/// A colour criterion's `<dxf>` index for a save: `(cell, rgb)` → the id of
/// an equal `<dxf>` the styles part has, or of one the save appends.
type DxfFor<'a> = dyn FnMut(bool, Option<(u8, u8, u8)>) -> Option<u32> + 'a;

/// Sync the sheet's `<autoFilter>` with the model. It is left byte-for-byte
/// alone while it holds the model's position and criteria, and dropped once
/// the model has none (a delete took its whole range, or the filter was
/// turned off). When only its position changed (a structural edit), it is
/// rewritten in place: a new `ref`, each `<filterColumn>` renumbered to the
/// column it filtered (or dropped with that column), everything else kept.
/// When its criteria changed, the element is written from the model, with
/// what we don't model ([`crate::filter::ColumnFilter::Raw`]) as it was.
/// Either rewrite drops the nested `<sortState>`, which would still name the
/// old cells. A model filter the part lacks (one the filter commands made, or
/// a restored sheet's) is added at its schema position. One whose `ref`
/// doesn't read as a range is left as it is.
fn set_auto_filter(
    mut xml: String,
    model: Option<&crate::sheet::SheetAutoFilter>,
    dxfs: &[crate::sheet::Dxf],
    dxf_for: &mut DxfFor,
) -> String {
    let Some((start, end)) = sheet_auto_filter_span(&xml) else {
        return match model {
            Some(af) => put_worksheet_child(
                &xml,
                "autoFilter",
                &auto_filter_block(af, dxf_for),
                None,
                false,
            ),
            None => xml,
        };
    };
    let element = &xml[start..end];
    let Some(held) = auto_filter_position(element, dxfs) else {
        return xml;
    };
    if model == Some(&held) {
        return xml;
    }
    let block = match model {
        None => String::new(),
        Some(af) => {
            // The criteria the part's columns would have if only their
            // positions moved.
            let doc = crate::filter::filter_columns(element, dxfs);
            let mut moved: Vec<(u32, crate::filter::ColumnFilter)> = doc
                .iter()
                .zip(&af.columns)
                .filter_map(|((_, f), c)| c.map(|c| (c, f.clone())))
                .collect();
            moved.sort_by_key(|(c, _)| *c);
            if doc.len() == af.columns.len() && moved == af.criteria {
                moved_auto_filter(element, af)
            } else {
                let block = auto_filter_block(af, dxf_for);
                match worksheet_root(&xml) {
                    Some(root) => in_worksheet_ns(&root, &block),
                    None => block,
                }
            }
        }
    };
    xml.replace_range(start..end, &block);
    xml
}

/// `element`, an `<autoFilter>` whose criteria are unchanged, moved to the
/// model's position: a new `ref`, each `<filterColumn>` renumbered or
/// dropped with its column, and no `<sortState>`.
fn moved_auto_filter(element: &str, af: &crate::sheet::SheetAutoFilter) -> String {
    let mut block = element.to_string();
    let mut column = 0;
    let mut edits: Vec<(usize, usize, Option<String>)> = Vec::new();
    for (name, s, e) in element_children(element) {
        match name.as_str() {
            "filterColumn" => {
                match af.columns.get(column) {
                    Some(None) => edits.push((s, e, None)),
                    Some(Some(c)) => {
                        let id = c.saturating_sub(af.range.1).to_string();
                        if attr_at(element, s, "colId") != Some(&id) {
                            edits.push((s, e, Some(id)));
                        }
                    }
                    None => {}
                }
                column += 1;
            }
            "sortState" => edits.push((s, e, None)),
            _ => {}
        }
    }
    for (s, e, id) in edits.into_iter().rev() {
        match id {
            Some(id) => block = set_tag_attr(&block, s, "colId", Some(&id)),
            None => block.replace_range(s..e, ""),
        }
    }
    let (r1, c1, r2, c2) = af.range;
    let r = format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2));
    set_tag_attr(&block, 0, "ref", Some(&r))
}

/// An `<autoFilter>` written from the model: its range and a
/// `<filterColumn>` per criterion inside it. A colour criterion with no
/// `<dxf>` of its own gets one from `dxf_for`.
fn auto_filter_block(af: &crate::sheet::SheetAutoFilter, dxf_for: &mut DxfFor) -> String {
    use crate::filter::ColumnFilter;
    let (r1, c1, r2, c2) = af.range;
    let mut cols = String::new();
    for (c, f) in &af.criteria {
        if *c < c1 || *c > c2 {
            continue;
        }
        let dxf_id = match f {
            ColumnFilter::Color { cell, rgb, dxf_id } => dxf_id.or_else(|| dxf_for(*cell, *rgb)),
            _ => None,
        };
        if let Some(x) = crate::filter::filter_column_xml(c - c1, f, dxf_id) {
            cols.push_str(&x);
        }
    }
    let r = format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2));
    if cols.is_empty() {
        format!("<autoFilter ref=\"{r}\"/>")
    } else {
        format!("<autoFilter ref=\"{r}\">{cols}</autoFilter>")
    }
}

/// `<sheetPr filterMode>`: whether the part says some rows are filtered.
fn read_filter_mode(xml: &str) -> bool {
    if !xml.contains("filterMode") {
        return false;
    }
    worksheet_child_span(xml, "sheetPr")
        .is_some_and(|(start, _)| matches!(attr_at(xml, start, "filterMode"), Some("1" | "true")))
}

/// Sync `<sheetPr filterMode>` (some rows of the sheet are filtered) with
/// what the filter commands last left; `None` leaves the part alone.
fn set_filter_mode(xml: String, mode: Option<bool>) -> String {
    let Some(now) = mode else {
        return xml;
    };
    let Some((start, _)) = worksheet_child_span(&xml, "sheetPr") else {
        if !now {
            return xml;
        }
        return put_worksheet_child(&xml, "sheetPr", "<sheetPr filterMode=\"1\"/>", None, false);
    };
    let was = matches!(attr_at(&xml, start, "filterMode"), Some("1" | "true"));
    if was == now {
        return xml;
    }
    let out = set_tag_attr(&xml, start, "filterMode", now.then_some("1"));
    // A `sheetPr` left with nothing goes.
    match worksheet_child_span(&out, "sheetPr") {
        Some((s, e)) if !now && page::is_bare(&out[s..e]) => {
            remove_worksheet_child(&out, "sheetPr")
        }
        _ => out,
    }
}

/// The `<dxf>` elements of a styles part's `<dxfs>`, in order.
fn dxf_elements(styles: &str) -> Vec<&str> {
    let Some(s) = styles.find("<dxfs") else {
        return Vec::new();
    };
    element_children(&styles[s..])
        .into_iter()
        .filter(|(n, _, _)| n == "dxf")
        .map(|(_, a, b)| &styles[s + a..s + b])
        .collect()
}

/// `styles` with `added` `<dxf>` elements appended to its `<dxfs>` (its
/// `count` bumped), or a new `<dxfs>` at its schema position. The existing
/// ones are left byte-for-byte.
fn append_dxfs(styles: &str, added: &[String]) -> String {
    if added.is_empty() {
        return styles.to_string();
    }
    let body: String = added.concat();
    if let Some(s) = styles.find("<dxfs") {
        let open_end = s + styles[s..].find('>').map_or(0, |i| i + 1);
        let have = dxf_elements(styles).len();
        let count = (have + added.len()).to_string();
        if styles[..open_end].ends_with("/>") {
            let block = format!("<dxfs count=\"{count}\">{body}</dxfs>");
            return format!("{}{block}{}", &styles[..s], &styles[open_end..]);
        }
        let Some(close) = styles[s..].find("</dxfs>").map(|i| s + i) else {
            return styles.to_string();
        };
        let out = format!("{}{body}{}", &styles[..close], &styles[close..]);
        return set_tag_attr(&out, s, "count", Some(&count));
    }
    let block = format!("<dxfs count=\"{}\">{body}</dxfs>", added.len());
    let anchor = ["<tableStyles", "<colors", "<extLst"]
        .iter()
        .find_map(|t| styles.find(t))
        .or_else(|| styles.find("</styleSheet>"));
    match anchor {
        Some(pos) => format!("{}{block}{}", &styles[..pos], &styles[pos..]),
        None => styles.to_string(),
    }
}

/// Sync one `<rowBreaks>` / `<colBreaks>` element from the model. It is left
/// byte-for-byte alone while it holds exactly the model's breaks, rewritten
/// (with its counts) when they differ, dropped when none are left, and
/// created at its schema position when breaks were inserted on a sheet that
/// had none.
fn set_page_breaks(mut xml: String, tag: &str, breaks: &[crate::sheet::PageBreak]) -> String {
    let Some((start, end)) = sheet_breaks_span(&xml, tag) else {
        if breaks.is_empty() {
            return xml;
        }
        return put_worksheet_child(&xml, tag, &page_breaks_block(tag, breaks), None, false);
    };
    if parse_page_breaks(&xml[start..end]) == breaks {
        return xml;
    }
    let block = if breaks.is_empty() {
        String::new()
    } else {
        let block = page_breaks_block(tag, breaks);
        match worksheet_root(&xml) {
            Some(root) => in_worksheet_ns(&root, &block),
            None => block,
        }
    };
    xml.replace_range(start..end, &block);
    xml
}

/// A `<rowBreaks>` / `<colBreaks>` element (unprefixed) holding `breaks`.
fn page_breaks_block(tag: &str, breaks: &[crate::sheet::PageBreak]) -> String {
    let manual = breaks.iter().filter(|b| b.is_manual()).count();
    let mut block = format!(
        "<{tag} count=\"{}\" manualBreakCount=\"{manual}\">",
        breaks.len()
    );
    for b in breaks {
        block.push_str(&format!("<brk id=\"{}\"{}/>", b.id, b.attrs));
    }
    block.push_str(&format!("</{tag}>"));
    block
}

/// Where the sheet's own `<rowBreaks>` / `<colBreaks>` element is (in any
/// prefix), found with the loader's parser so a comment can't end it early.
/// Custom views hold breaks under the same names inside `<customSheetViews>`;
/// only a top-level one counts.
fn sheet_breaks_span(xml: &str, tag: &str) -> Option<(usize, usize)> {
    // Most sheets have no breaks: don't walk the part to learn that.
    if !xml.contains(tag) {
        return None;
    }
    worksheet_child_span(xml, tag)
}

/// The `<brk>` children of one breaks element, read as the loader reads them.
fn parse_page_breaks(fragment: &str) -> Vec<crate::sheet::PageBreak> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(fragment);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == "brk" => out.extend(page_break(&p)),
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// The children of `<worksheet>` in the order ECMA-376 `CT_Worksheet` (a
/// SEQUENCE) requires them. Excel treats an out-of-order child as damage and
/// offers to "repair" the file, which drops content.
const CT_WORKSHEET_ORDER: &[&str] = &[
    "sheetPr",
    "dimension",
    "sheetViews",
    "sheetFormatPr",
    "cols",
    "sheetData",
    "sheetCalcPr",
    "sheetProtection",
    "protectedRanges",
    "scenarios",
    "autoFilter",
    "sortState",
    "dataConsolidate",
    "customSheetViews",
    "mergeCells",
    "phoneticPr",
    "conditionalFormatting",
    "dataValidations",
    "hyperlinks",
    "printOptions",
    "pageMargins",
    "pageSetup",
    "headerFooter",
    "rowBreaks",
    "colBreaks",
    "customProperties",
    "cellWatches",
    "ignoredErrors",
    "smartTags",
    "drawing",
    "legacyDrawing",
    "legacyDrawingHF",
    "drawingHF",
    "picture",
    "oleObjects",
    "controls",
    "webPublishItems",
    "tableParts",
    "extLst",
];

/// A top-level child of `<worksheet>`: its local name, the name it ranks as
/// (they differ for `mc:AlternateContent`), and its byte span.
struct WorksheetChild<'a> {
    local: &'a str,
    rank_as: &'a str,
    start: usize,
    end: usize,
}

/// What a walk over a worksheet's top-level children found.
struct WorksheetWalk<'a> {
    /// The children, in order, each a complete element.
    children: Vec<WorksheetChild<'a>>,
    /// Where content after the last child goes: `</worksheet>`, or the end
    /// of the last child when the part just stops there. `Some` only when
    /// `children` is the whole list.
    close: Option<usize>,
    /// The child the walk could not read to its end: its start and local
    /// name. Its start tag parsed, so its position is known even though its
    /// body isn't; the walk saw nothing after it.
    stopped: Option<(usize, &'a str)>,
}

/// The top-level children of a worksheet, and where `</worksheet>` starts.
/// Only depth-1 elements count (the `<autoFilter>` inside a
/// `<customSheetView>` is not one), compared by local name so `x:mergeCells`
/// is `mergeCells`. A top-level `mc:AlternateContent` ranks as the first
/// element in its first `Choice`/`Fallback`: Excel wraps `controls`,
/// `oleObjects` and `legacyDrawing` that way.
///
/// A child the parser can't read to its end (a mismatched end tag inside
/// it) is spanned by its own end tag when one follows and
/// [`resync_is_safe`] vouches for it, and the walk goes on. Otherwise the
/// walk stops there and records it in `stopped`. An end tag that isn't
/// `</worksheet>` also stops the walk, with `stopped` left `None`. Either
/// way `close` is `None`: the children before the stop are real, but the
/// list is not known to be whole. `close` is `</worksheet>`'s start, or the
/// end of the last child when the part ends without one.
/// [`worksheet_insert_pos`] says where an insert is still safe; a
/// self-closing root has nothing to walk at all.
fn worksheet_children(xml: &str) -> WorksheetWalk<'_> {
    let mut walk = WorksheetWalk {
        children: Vec::new(),
        close: None,
        stopped: None,
    };
    let mut p = XmlParser::new(xml);
    // Skip to the root's start tag.
    loop {
        match p.next() {
            Event::Start => break,
            Event::Eof => return walk,
            _ => {}
        }
    }
    if xml[..p.pos()].ends_with("/>") {
        return walk;
    }
    let root = p.name();
    let root_end = p.pos();
    // Offset of `p`'s input within `xml`: the walk restarts past a sheetData
    // it skipped and past a child it had to span by its end tag.
    let mut base = 0;
    loop {
        match p.next() {
            Event::Start => {
                let start = base + p.start_pos();
                let qname = p.name();
                let name = local(qname);
                let body = base + p.pos();
                let self_closing = xml[..body].ends_with("/>");
                let skip_to = (name == "sheetData" && !self_closing)
                    .then(|| sheet_data_end(xml, body, qname))
                    .flatten();
                let end = match skip_to {
                    Some(end) => Some(end),
                    None if p.skip_element_complete() => Some(base + p.pos()),
                    // Broken inside: its own end tag, if one follows, still
                    // bounds it, unless it could be someone else's.
                    None => close_tag_end(xml, body, qname)
                        .filter(|&end| resync_is_safe(&xml[body..end], name)),
                };
                let Some(end) = end else {
                    walk.stopped = Some((start, name));
                    return walk;
                };
                if skip_to.is_some() || base + p.pos() != end {
                    // The bulk of the part, or a damaged child: resume past it
                    // rather than tokenise the cells or trust the parser's
                    // position.
                    base = end;
                    p = XmlParser::new(&xml[end..]);
                }
                let rank_as = match name {
                    "AlternateContent" => alternate_content_rank(&xml[start..end]).unwrap_or(name),
                    _ => name,
                };
                walk.children.push(WorksheetChild {
                    local: name,
                    rank_as,
                    start,
                    end,
                });
            }
            // `</worksheet>`: the end tag just consumed. A stray end tag of
            // another name means the structure is off: stop.
            Event::End => {
                if p.name() == root {
                    walk.close = xml[..base + p.pos()].rfind("</");
                }
                return walk;
            }
            // No `</worksheet>`: every child was read, so the list is whole;
            // new content goes after the last one.
            Event::Eof => {
                walk.close = Some(walk.children.last().map_or(root_end, |c| c.end));
                return walk;
            }
            Event::Text => {}
        }
    }
}

/// The worksheet's `<sheetData>` (any prefix) for a part whose top-level
/// walk stopped before it: read with the parser, or, when the parser can't
/// finish it, spanned by its end tag as long as no comment, CDATA section or
/// processing instruction could be hiding a literal one. `None` leaves the
/// part as it was, which is what main did with a sheetData it couldn't find.
fn sheet_data_fallback_span(xml: &str) -> Option<(usize, usize)> {
    if let Some(span) = element_span_by_tags(xml, "sheetData") {
        return Some(span);
    }
    let (start, prefix) = find_local_element(xml, "sheetData")?;
    let gt = tag_end(xml, start);
    if xml[..gt].ends_with("/>") {
        return Some((start, gt));
    }
    Some((
        start,
        sheet_data_end(xml, gt, &format!("{prefix}sheetData"))?,
    ))
}

/// Can the end tag that closes `span` be trusted to be its broken child's
/// own? Not when a comment, CDATA section or processing instruction could
/// hold a literal one, nor when an element of the same local name nests in
/// it (a `<customSheetView>`'s `<autoFilter>` inside a broken top-level one):
/// its end tag would close that instead.
fn resync_is_safe(span: &str, name: &str) -> bool {
    if span.contains("<!--") || span.contains("<![CDATA[") || span.contains("<?") {
        return false;
    }
    let mut p = XmlParser::new(span);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == name => return false,
            Event::Eof => return true,
            _ => {}
        }
    }
}

/// Just past the first `</qname>` (any spacing before `>`) at or after `from`.
fn close_tag_end(xml: &str, from: usize, qname: &str) -> Option<usize> {
    let needle = format!("</{qname}");
    let mut from = from;
    loop {
        let at = from + xml[from..].find(&needle)?;
        let rest = &xml[at + needle.len()..];
        let trimmed = rest.trim_start_matches([' ', '\t', '\r', '\n']);
        if trimmed.starts_with('>') {
            return Some(at + needle.len() + (rest.len() - trimmed.len()) + 1);
        }
        from = at + needle.len();
    }
}

/// The first element whose local name is `tag`, anywhere in the part, read
/// with the parser: for a worksheet whose top-level walk never reached it.
/// sheetData, mergeCells and sheetProtection appear nowhere else, so the
/// first one is the worksheet's own.
fn element_span_by_tags(xml: &str, tag: &str) -> Option<(usize, usize)> {
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == tag => {
                let start = p.start_pos();
                return p.skip_element_complete().then(|| (start, p.pos()));
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// Just past the `</sheetData>` (any spacing before `>`) that closes the
/// sheetData whose body starts at `body`, found without tokenising the cells.
/// `None` sends the caller to the parser: no such tag, or a comment, CDATA
/// section or processing instruction in the way that could hold a literal
/// one.
fn sheet_data_end(xml: &str, body: usize, qname: &str) -> Option<usize> {
    let close = close_tag_end(xml, body, qname)?;
    let span = &xml[body..close];
    (!span.contains("<!--") && !span.contains("<![CDATA[") && !span.contains("<?")).then_some(close)
}

/// The local name of the first element inside the first `Choice` or
/// `Fallback` of an `mc:AlternateContent` fragment.
fn alternate_content_rank(fragment: &str) -> Option<&str> {
    let mut p = XmlParser::new(fragment);
    let mut in_branch = false;
    loop {
        match p.next() {
            Event::Start if in_branch => return Some(local(p.name())),
            Event::Start if matches!(local(p.name()), "Choice" | "Fallback") => in_branch = true,
            Event::End if in_branch => return None,
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn ct_worksheet_rank(name: &str) -> Option<usize> {
    CT_WORKSHEET_ORDER.iter().position(|n| *n == name)
}

/// Byte offset at which a new top-level `<tag>` belongs: before the first
/// existing top-level child that ranks after `tag`, else at the walk's
/// `close` (`</worksheet>`, or the end of the last child when the part has
/// none). Children the schema doesn't name are not anchors.
///
/// After a walk that stopped, the position is still known when a found
/// child ranks after `tag`, or when the child it stopped at does (its start
/// tag parsed). Otherwise it is `None`: there is no safe place.
///
/// A `Some` also means `tag` was looked for everywhere it may stand: in a
/// schema-ordered part an element ranking before the stopped child can't
/// follow it. So a caller that didn't find a singleton may add it here
/// without risking a second.
fn worksheet_insert_pos(xml: &str, tag: &str) -> Option<usize> {
    let rank = ct_worksheet_rank(tag).unwrap_or(CT_WORKSHEET_ORDER.len());
    let after = |name: &str| ct_worksheet_rank(name).is_some_and(|r| r > rank);
    let walk = worksheet_children(xml);
    walk.children
        .iter()
        .find(|c| after(c.rank_as))
        .map(|c| c.start)
        .or(walk.close)
        .or_else(|| walk.stopped.filter(|&(_, n)| after(n)).map(|(s, _)| s))
}

/// Can a `<tag>` be added to this worksheet: is its position known, or
/// (with `join`) is there one to join or replace? The edit APIs ask first,
/// so an edit that can't be written changes neither the part nor the model.
fn worksheet_takes(xml: &str, tag: &str, join: bool) -> bool {
    // A self-closing root is opened up by the insert.
    worksheet_root(xml).is_some_and(|r| r.self_closing)
        || (join && worksheet_child_span(xml, tag).is_some())
        || worksheet_insert_pos(xml, tag).is_some()
}

/// The span of the first top-level `<tag>` (in any prefix) the walk found.
/// `None` does not prove there is none when the walk stopped early; the
/// insert paths check that for themselves.
pub(crate) fn worksheet_child_span(xml: &str, tag: &str) -> Option<(usize, usize)> {
    worksheet_children(xml)
        .children
        .iter()
        .find(|c| c.local == tag)
        .map(|c| (c.start, c.end))
}

/// `xml` with its top-level `<tag>` (if the walk found one) removed.
pub(crate) fn remove_worksheet_child(xml: &str, tag: &str) -> String {
    match worksheet_child_span(xml, tag) {
        Some((s, e)) => format!("{}{}", &xml[..s], &xml[e..]),
        None => xml.to_string(),
    }
}

/// `xml` without its singleton top-level `<tag>`: the one the walk found, or,
/// when the walk stopped where a `<tag>` could still follow, the first one
/// found by its tags (what main did). The tag scan runs only in that case:
/// the plain save of a sheet with no merges asks this on every save.
pub(crate) fn remove_worksheet_singleton(xml: &str, tag: &str) -> String {
    if worksheet_child_span(xml, tag).is_some() {
        return remove_worksheet_child(xml, tag);
    }
    if worksheet_insert_pos(xml, tag).is_some() {
        return xml.to_string();
    }
    match element_span_by_tags(xml, tag) {
        Some((s, e)) => format!("{}{}", &xml[..s], &xml[e..]),
        None => xml.to_string(),
    }
}

/// Sync a singleton top-level `<tag>` with `block` (`None` drops it), at
/// save time, where the model has to reach the part.
///
/// The old element (found by the walk) goes, and the new one lands at its
/// schema position, which also repairs a misplaced one. Where
/// [`worksheet_insert_pos`] has no position (the walk stopped at or before
/// `tag`'s rank), it does what main did: the first `<tag>` anywhere is
/// replaced, or the block goes right after `</sheetData>`. Never a second
/// one, and never a model edit that the part doesn't carry.
fn sync_worksheet_child(xml: &str, tag: &str, block: Option<&str>) -> String {
    let Some(block) = block else {
        return remove_worksheet_singleton(xml, tag);
    };
    let out = remove_worksheet_child(xml, tag);
    if worksheet_insert_pos(&out, tag).is_some() {
        return put_worksheet_child(&out, tag, block, None, false);
    }
    let block = match worksheet_root(xml) {
        Some(root) => in_worksheet_ns(&root, block),
        None => block.to_string(),
    };
    let (s, e) = match worksheet_child_span(xml, tag).or_else(|| element_span_by_tags(xml, tag)) {
        Some(span) => span,
        None => match element_span_by_tags(xml, "sheetData") {
            Some((_, e)) => (e, e),
            // Nowhere to anchor it at all: the part is too damaged to take it.
            None => return xml.to_string(),
        },
    };
    format!("{}{block}{}", &xml[..s], &xml[e..])
}

/// What a worksheet's root start tag binds. The writer writes its elements
/// unprefixed and `r:id` with the `r` prefix; these say whether that is
/// already right where they land.
struct WorksheetRoot {
    /// Offset of the root start tag's closing `>` (or `/>`).
    tag_close: usize,
    /// `<worksheet …/>`: no content, and no end tag to insert before.
    self_closing: bool,
    qname: String,
    /// The namespace the root element itself is in.
    ns: String,
    /// The default namespace the root declares, if any.
    default_ns: Option<String>,
    /// What the root binds the `r` prefix to, if anything.
    r_ns: Option<String>,
}

fn worksheet_root(xml: &str) -> Option<WorksheetRoot> {
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => break,
            Event::Eof => return None,
            _ => {}
        }
    }
    let bound = |decl: &str| {
        p.namespace_attrs()
            .iter()
            .rev()
            .find(|a| a.name == decl)
            .map(|a| a.value.to_string())
    };
    let own = match p.name().split_once(':') {
        Some((prefix, _)) => format!("xmlns:{prefix}"),
        None => "xmlns".to_string(),
    };
    let end = p.pos();
    let self_closing = xml[..end].ends_with("/>");
    Some(WorksheetRoot {
        tag_close: if self_closing { end - 2 } else { end - 1 },
        self_closing,
        qname: p.name().to_string(),
        ns: bound(&own).unwrap_or_default(),
        default_ns: bound("xmlns"),
        r_ns: bound("xmlns:r"),
    })
}

/// `block` with ` {decl}` added to its first start tag, after the name.
fn with_decl(block: &str, decl: &str) -> String {
    let name_end = block
        .char_indices()
        .skip(1)
        .find(|&(_, c)| c.is_whitespace() || c == '/' || c == '>')
        .map_or(block.len(), |(i, _)| i);
    format!("{} {decl}{}", &block[..name_end], &block[name_end..])
}

/// `block` (written unprefixed) as it must be spelled inside this worksheet:
/// where the default namespace isn't the worksheet's own (a sheet written as
/// `<x:worksheet xmlns:x="…">`), the block declares it, so its unprefixed
/// names stay in SpreadsheetML.
fn in_worksheet_ns(root: &WorksheetRoot, block: &str) -> String {
    if root.default_ns.as_deref() == Some(root.ns.as_str()) || root.ns.is_empty() {
        block.to_string()
    } else {
        with_decl(block, &format!("xmlns=\"{}\"", root.ns))
    }
}

/// Make `r:` mean `rels` for `block`: declared on the root when the root
/// doesn't bind `r`, or on the block itself when the root binds it to
/// something else.
fn bind_r(xml: &mut String, root: &WorksheetRoot, block: String, rels: &str) -> String {
    match root.r_ns.as_deref() {
        Some(r) if r == rels => block,
        None => {
            xml.insert_str(root.tag_close, &format!(" xmlns:r=\"{rels}\""));
            block
        }
        Some(_) => with_decl(&block, &format!("xmlns:r=\"{rels}\"")),
    }
}

/// Put `block`, a top-level `<tag>` element written unprefixed, into the
/// worksheet: in place of the existing `<tag>` (in any prefix) when
/// `replace` is set and there is one, else at its `CT_Worksheet` position.
/// With `rels`, the block's `r:id` is bound to that namespace. A replace
/// works whenever the walk found the element; an insert needs a position
/// [`worksheet_insert_pos`] vouches for, and otherwise the part is returned
/// unchanged.
pub(crate) fn put_worksheet_child(
    xml: &str,
    tag: &str,
    block: &str,
    rels: Option<&str>,
    replace: bool,
) -> String {
    let mut out = xml.to_string();
    let Some(root) = worksheet_root(xml) else {
        return out;
    };
    if root.self_closing {
        // `<worksheet …/>`: open it up, so the block goes inside the root
        // rather than after the document element.
        out.replace_range(
            root.tag_close..root.tag_close + 2,
            &format!("></{}>", root.qname),
        );
    }
    let mut block = in_worksheet_ns(&root, block);
    if let Some(rels) = rels {
        block = bind_r(&mut out, &root, block, rels);
    }
    match worksheet_child_span(&out, tag).filter(|_| replace) {
        Some((s, e)) => out.replace_range(s..e, &block),
        None => match worksheet_insert_pos(&out, tag) {
            Some(pos) => out.insert_str(pos, &block),
            // No known position: the part is left as it is. The edit APIs
            // ask `worksheet_takes` first, so they refuse instead.
            None => return xml.to_string(),
        },
    }
    out
}

/// Append `item` inside the worksheet's existing top-level `<tag>` (in any
/// prefix), bumping its `count`. `None` when there is no such element.
pub(crate) fn append_to_worksheet_child(
    xml: &str,
    tag: &str,
    item: &str,
    rels: Option<&str>,
) -> Option<String> {
    worksheet_child_span(xml, tag)?;
    let mut out = xml.to_string();
    let root = worksheet_root(xml)?;
    let mut item = in_worksheet_ns(&root, item);
    if let Some(rels) = rels {
        item = bind_r(&mut out, &root, item, rels);
    }
    insert_into_worksheet_child(out, tag, &item, 1)
}

/// [`append_to_worksheet_child`] for several items at once (none binding
/// `r:`): one rewrite of the part, where an append each would rewrite it
/// once per item (#707 r6).
pub(crate) fn append_all_to_worksheet_child(
    xml: &str,
    tag: &str,
    items: &[String],
) -> Option<String> {
    worksheet_child_span(xml, tag)?;
    let root = worksheet_root(xml)?;
    let item: String = items.iter().map(|i| in_worksheet_ns(&root, i)).collect();
    insert_into_worksheet_child(xml.to_string(), tag, &item, items.len() as u32)
}

/// `item` (`added` elements) put at the end of the worksheet's `<tag>`
/// child, its `count` bumped by them.
fn insert_into_worksheet_child(
    mut out: String,
    tag: &str,
    item: &str,
    added: u32,
) -> Option<String> {
    let (s, _) = worksheet_child_span(&out, tag)?;
    if let Some(n) = attr_at(&out, s, "count").and_then(|v| v.parse::<u32>().ok()) {
        out = set_tag_attr(&out, s, "count", Some(&(n + added).to_string()));
    }
    let (s, e) = worksheet_child_span(&out, tag)?;
    if out[..e].ends_with("/>") {
        // `<x:tableParts count="0"/>`: open it up.
        let qname_end = out[s + 1..]
            .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .map_or(e, |i| s + 1 + i);
        let qname = out[s + 1..qname_end].to_string();
        out.replace_range(e - 2..e, &format!(">{item}</{qname}>"));
    } else {
        let close = out[..e].rfind("</").unwrap_or(e);
        out.insert_str(close, item);
    }
    Some(out)
}

/// Sync the `<sheetProtection>` element from the model: the existing one
/// goes, and the sheet's protection lands at its schema position, or where
/// [`sync_worksheet_child`] can still put it on a malformed part. Idempotent.
fn set_sheet_protection(xml: &str, attrs: Option<&str>) -> String {
    let block = attrs.map(|a| format!("<sheetProtection {a}/>"));
    sync_worksheet_child(xml, "sheetProtection", block.as_deref())
}

/// Rewrite the `<mergeCells>` block from the model's merged regions (removing it
/// when there are none), at its schema position, or where
/// [`sync_worksheet_child`] can still put it on a malformed part. Idempotent.
fn set_merge_cells(xml: &str, merges: &[(u32, u32, u32, u32)]) -> String {
    if merges.is_empty() {
        return sync_worksheet_child(xml, "mergeCells", None);
    }
    let cells: String = merges
        .iter()
        .map(|&(r1, c1, r2, c2)| {
            format!(
                "<mergeCell ref=\"{}:{}\"/>",
                cell_name(r1, c1),
                cell_name(r2, c2)
            )
        })
        .collect();
    let block = format!(
        "<mergeCells count=\"{}\">{cells}</mergeCells>",
        merges.len()
    );
    sync_worksheet_child(xml, "mergeCells", Some(&block))
}

/// A `<dxf>` (differential format) for conditional formatting.
fn dxf_to_xml(dxf: &crate::sheet::Dxf) -> String {
    let mut font = String::new();
    if dxf.bold == Some(true) {
        font.push_str("<b/>");
    }
    if dxf.italic == Some(true) {
        font.push_str("<i/>");
    }
    if let Some((r, g, b)) = dxf.color {
        font.push_str(&format!("<color rgb=\"FF{r:02X}{g:02X}{b:02X}\"/>"));
    }
    let font = if font.is_empty() {
        String::new()
    } else {
        format!("<font>{font}</font>")
    };
    let fill = match dxf.fill {
        Some((r, g, b)) => format!(
            "<fill><patternFill patternType=\"solid\"><bgColor rgb=\"FF{r:02X}{g:02X}{b:02X}\"/></patternFill></fill>"
        ),
        None => String::new(),
    };
    format!("<dxf>{font}{fill}</dxf>")
}

/// Regenerate the `<dxfs>` block of `styles.xml` from the model's dxfs (replacing
/// any existing block, or inserting one before tableStyles/colors/extLst/end).
fn set_dxfs(xml: &str, dxfs: &[crate::sheet::Dxf]) -> String {
    let body: String = dxfs.iter().map(dxf_to_xml).collect();
    let block = format!("<dxfs count=\"{}\">{body}</dxfs>", dxfs.len());
    if let Some(s) = xml.find("<dxfs") {
        let e = xml[s..]
            .find("</dxfs>")
            .map(|i| s + i + "</dxfs>".len())
            .or_else(|| xml[s..].find("/>").map(|i| s + i + 2))
            .unwrap_or(s);
        return format!("{}{block}{}", &xml[..s], &xml[e..]);
    }
    // Insert at the first schema-valid anchor after cellXfs/cellStyles.
    let anchor = ["<tableStyles", "<colors", "<extLst"]
        .iter()
        .find_map(|t| xml.find(t))
        .or_else(|| xml.find("</styleSheet>"));
    match anchor {
        Some(pos) => format!("{}{block}{}", &xml[..pos], &xml[pos..]),
        None => xml.to_string(),
    }
}

/// Rewrite the frozen-pane state of the first `<sheetView>` from the model's
/// `freeze` (rows, cols): inserts/updates `<pane … state="frozen"/>`, removes it
/// when unfrozen, and creates a `<sheetViews>` block if the worksheet lacks one.
/// Idempotent — a second save with the same freeze is byte-identical.
fn set_freeze_pane(xml: &str, freeze: (u32, u32)) -> String {
    let (fr, fc) = freeze;
    let pane = if fr == 0 && fc == 0 {
        String::new()
    } else {
        let mut a = String::new();
        if fc > 0 {
            a.push_str(&format!(" xSplit=\"{fc}\""));
        }
        if fr > 0 {
            a.push_str(&format!(" ySplit=\"{fr}\""));
        }
        format!(
            "<pane{a} topLeftCell=\"{}\" activePane=\"bottomRight\" state=\"frozen\"/>",
            cell_name(fr, fc)
        )
    };
    let new_views =
        || format!("<sheetViews><sheetView workbookViewId=\"0\">{pane}</sheetView></sheetViews>");
    let Some(view) = first_sheet_view(xml) else {
        // No <sheetView> (in any prefix): a full block at its schema position,
        // in place of an empty <sheetViews/> if there is one.
        return if pane.is_empty() {
            xml.to_string()
        } else {
            put_worksheet_child(xml, "sheetViews", &new_views(), None, true)
        };
    };
    // Drop its existing <pane> first (idempotent; also handles unfreeze).
    let mut out = xml.to_string();
    if let Some((ps, pe)) = view.pane {
        out.replace_range(ps..pe, "");
    }
    if pane.is_empty() {
        return out;
    }
    let pane = match worksheet_root(&out) {
        Some(root) => in_worksheet_ns(&root, &pane),
        None => pane,
    };
    // A pane is the sheetView's first child: right after its start tag,
    // expanding a self-closing one.
    if view.self_closing {
        let gt = view.tag_end - 2;
        out.replace_range(gt..view.tag_end, &format!(">{pane}</{}>", view.qname));
    } else {
        out.insert_str(view.tag_end, &pane);
    }
    out
}

/// The worksheet's first `<sheetView>` (in any prefix, inside the top-level
/// `<sheetViews>`, never one in `<customSheetViews>`) and its `<pane>`.
struct SheetViewAt {
    /// Just past its start tag.
    tag_end: usize,
    qname: String,
    self_closing: bool,
    pane: Option<(usize, usize)>,
}

fn first_sheet_view(xml: &str) -> Option<SheetViewAt> {
    let (vs, ve) = worksheet_child_span(xml, "sheetViews")?;
    let mut p = XmlParser::new(&xml[vs..ve]);
    // The <sheetViews> start tag, then its first <sheetView> child.
    matches!(p.next(), Event::Start).then_some(())?;
    loop {
        match p.next() {
            Event::Start if local(p.name()) == "sheetView" => break,
            Event::Start => {
                p.skip_element_complete();
            }
            Event::End | Event::Eof => return None,
            Event::Text => {}
        }
    }
    let qname = p.name().to_string();
    let tag_end = vs + p.pos();
    let self_closing = xml[..tag_end].ends_with("/>");
    let mut pane = None;
    if !self_closing {
        loop {
            match p.next() {
                Event::Start => {
                    let start = vs + p.start_pos();
                    let is_pane = local(p.name()) == "pane";
                    if !p.skip_element_complete() {
                        break;
                    }
                    if is_pane {
                        pane = Some((start, vs + p.pos()));
                        break;
                    }
                }
                Event::End | Event::Eof => break,
                Event::Text => {}
            }
        }
    }
    Some(SheetViewAt {
        tag_end,
        qname,
        self_closing,
        pane,
    })
}

/// The start tag beginning at `start`: its end (just past `>`).
fn tag_end(xml: &str, start: usize) -> usize {
    xml[start..].find('>').map_or(xml.len(), |i| start + i + 1)
}

/// The value of `attr` on the start tag beginning at `start`.
fn attr_at<'a>(xml: &'a str, start: usize, attr: &str) -> Option<&'a str> {
    tag_attr(&xml[start..tag_end(xml, start)], attr)
}

/// The start tag beginning at `start` with `attr` set to `value`, or removed
/// when `value` is `None`. A new attribute follows the element name.
fn set_tag_attr(xml: &str, start: usize, attr: &str, value: Option<&str>) -> String {
    let end = tag_end(xml, start);
    let mut tag = xml[start..end].to_string();
    // Every spelling of it goes (`a="1"`, `a = '1'`), so none is duplicated.
    while let Some((ws, _, _, after)) = attr_span(&tag, attr) {
        tag.replace_range(ws..after, "");
    }
    if let Some(v) = value {
        let name_end = tag
            .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .unwrap_or(tag.len());
        tag.insert_str(name_end, &format!(" {attr}=\"{v}\""));
    }
    format!("{}{tag}{}", &xml[..start], &xml[end..])
}

/// The first element named `local` in any namespace prefix (`workbookView`
/// or `x:workbookView`): its start and its `prefix:` (empty when none).
fn find_local_element<'a>(xml: &'a str, local: &str) -> Option<(usize, &'a str)> {
    let mut from = 0;
    while let Some(off) = xml[from..].find('<') {
        let at = from + off;
        from = at + 1;
        let rest = &xml[at + 1..];
        if rest.starts_with(['/', '?', '!']) {
            continue;
        }
        let name_len = rest
            .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .unwrap_or(rest.len());
        let name = &rest[..name_len];
        let (prefix, bare) = match name.rfind(':') {
            Some(i) => (&rest[..i + 1], &name[i + 1..]),
            None => ("", name),
        };
        if bare == local {
            return Some((at, prefix));
        }
    }
    None
}

/// Sync `<workbookView activeTab>` with the model's active sheet, adding
/// `<bookViews>` (before `<sheets>`, in its namespace prefix) only when a
/// sheet other than the first is active.
fn set_active_tab(xml: &str, active: usize) -> String {
    if let Some((i, _)) = find_local_element(xml, "workbookView") {
        let current = attr_at(xml, i, "activeTab").and_then(|v| v.parse().ok());
        if current.unwrap_or(0) == active {
            return xml.to_string();
        }
        let out = set_tag_attr(xml, i, "activeTab", Some(&active.to_string()));
        // A first visible tab past the active one would hide it.
        let first = attr_at(&out, i, "firstSheet").and_then(|v| v.parse::<usize>().ok());
        if first.is_some_and(|f| f > active) {
            return set_tag_attr(&out, i, "firstSheet", None);
        }
        return out;
    }
    if active == 0 {
        return xml.to_string();
    }
    match find_local_element(xml, "sheets") {
        Some((i, p)) => {
            let mut out = xml.to_string();
            out.insert_str(
                i,
                &format!("<{p}bookViews><{p}workbookView activeTab=\"{active}\"/></{p}bookViews>"),
            );
            out
        }
        None => xml.to_string(),
    }
}

/// Does the worksheet's first `<sheetView>` (in any prefix) say
/// `tabSelected="1"`, however it is spelled?
fn tab_is_selected(xml: &str) -> bool {
    find_local_element(xml, "sheetView")
        .is_some_and(|(i, _)| matches!(attr_at(xml, i, "tabSelected"), Some("1" | "true")))
}

/// Mark the first `<sheetView>` selected (`tabSelected="1"`) or not. Excel
/// opens every selected tab as a group, so only the active sheet may carry
/// it. A sheet without a `<sheetView>` is left alone.
fn set_tab_selected(xml: &str, selected: bool) -> String {
    let Some((i, _)) = find_local_element(xml, "sheetView") else {
        return xml.to_string();
    };
    if tab_is_selected(xml) == selected {
        return xml.to_string();
    }
    set_tag_attr(xml, i, "tabSelected", selected.then_some("1"))
}

/// Rewrite the `name` attribute of each `<sheet …>` element (in document
/// order) from the model's sheet names.
fn patch_sheet_names(xml: &str, sheets: &[Sheet]) -> String {
    // Positional patching is only safe when the `<sheet>` elements line up
    // one-to-one with the model. They can diverge if a worksheet part was
    // missing at load and its sheet was dropped from the model; renaming by
    // index would then write names onto the wrong elements. Bail out (leaving
    // the original names) rather than corrupt them.
    if xml.matches("<sheet ").count() != sheets.len() {
        return xml.to_string();
    }
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    let mut idx = 0usize;
    while let Some(pos) = rest.find("<sheet ") {
        let (head, tail) = rest.split_at(pos);
        out.push_str(head);
        let elem_end = tail.find('>').map(|i| i + 1).unwrap_or(tail.len());
        let elem = &tail[..elem_end];
        if let (Some(sheet), Some(ns)) = (sheets.get(idx), elem.find("name=\"")) {
            let vs = ns + "name=\"".len();
            if let Some(ve) = elem[vs..].find('"') {
                out.push_str(&elem[..vs]);
                out.push_str(&esc_attr(&sheet.name));
                out.push_str(&elem[vs + ve..]);
            } else {
                out.push_str(elem);
            }
        } else {
            out.push_str(elem);
        }
        idx += 1;
        rest = &tail[elem_end..];
    }
    out.push_str(rest);
    out
}

/// Write each `<definedName>`'s definition back from the model where it
/// differs, replacing only the element's content. Elements are found with the
/// loader's parser, and their text compared as the loader read it, so CDATA
/// and comments neither look like changes nor cut an element short. A
/// `localSheetId` names a model sheet only while the `<sheet>` elements line
/// up with the model one-to-one (a sheet whose part was missing at load
/// breaks that, as in `patch_sheet_names`); otherwise scoped names are left
/// as they are. So is a name whose (name, scope) more than one model entry
/// shares: the loader demotes an unresolvable scope to global, and other
/// writers repeat names, so which entry belongs to which element can't be
/// told. An element with child elements is left alone too.
///
/// A model name with no element (a removed sheet's names, restored by undo)
/// is written as a new one at the end of `<definedNames>`, which is created
/// if the workbook has none. Never a second element for one name: a global
/// name only when no element of that name exists in any scope (the loader
/// makes an unresolvable scope global, so its element carries some other
/// `localSheetId`), a scoped one only when the scopes line up and no element
/// has that name and scope. `_xlnm._FilterDatabase` is added only for a
/// sheet in `filtered` (one with an AutoFilter, or rows an Advanced Filter
/// hid), and hidden as Excel writes it: it backs that filter, and a stray
/// one would make Excel see a filter that isn't there.
///
/// One kind of element is removed: a sheet's print area, print titles or
/// `_FilterDatabase` that the model no longer holds, under the same
/// alignment rule (see [`patch_defined_name`]); the filter commands drop
/// `_FilterDatabase` when the filter is turned off.
fn patch_defined_names(
    xml: &str,
    names: &[DefinedName],
    sheet_count: usize,
    filtered: &[bool],
) -> String {
    let aligned = xml.matches("<sheet ").count() == sheet_count;
    // (start, end, replacement): element contents, and where new names go.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    // The (lowercased name, key) of every element; key `None` = scope unknown.
    let mut seen: Vec<(String, Option<Option<usize>>)> = Vec::new();
    // Where new elements go: before `</definedNames>` (with the element's
    // prefix), or replacing a self-closing `<definedNames/>`.
    let mut names_close: Option<(usize, usize, String, bool)> = None;
    // Past the last `<sheets>` / `<functionGroups>` / `<externalReferences>`
    // (with the root's prefix): where a missing `<definedNames>` goes.
    let mut names_slot: Option<(usize, String)> = None;
    let mut root_prefix = String::new();
    let mut depth = 0usize;
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => {
                depth += 1;
                let prefix = match p.name().rsplit_once(':') {
                    Some((pfx, _)) => format!("{pfx}:"),
                    None => String::new(),
                };
                match (depth, local(p.name())) {
                    (1, _) => root_prefix = prefix,
                    (2, "definedNames") => {
                        let end = p.pos();
                        if xml[..end].ends_with("/>") {
                            names_close = Some((p.start_pos(), end, prefix, true));
                        }
                    }
                    (_, "definedName") => {
                        let key = defined_name_key(&p, aligned);
                        seen.push((
                            decode(p.attr("name")).to_lowercase(),
                            key.as_ref().map(|k| k.1),
                        ));
                        let body_start = p.pos();
                        if xml[..body_start].ends_with("/>") {
                            continue;
                        }
                        depth -= 1; // its end tag is consumed here
                        let ctx = NameCtx {
                            start: p.start_pos(),
                            sheet_count,
                        };
                        if !patch_defined_name(xml, &mut p, key, names, ctx, &mut edits) {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            Event::End => {
                let at = xml[..p.pos()].rfind("</").unwrap_or(p.pos());
                match (depth, local(p.name())) {
                    (2, "definedNames") if names_close.is_none() => {
                        let prefix = match p.name().rsplit_once(':') {
                            Some((pfx, _)) => format!("{pfx}:"),
                            None => String::new(),
                        };
                        names_close = Some((at, at, prefix, false));
                    }
                    (2, "sheets" | "functionGroups" | "externalReferences") => {
                        names_slot = Some((p.pos(), root_prefix.clone()));
                    }
                    _ => {}
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            Event::Text => {}
        }
    }

    // Model names no element accounts for, each (name, scope) once.
    let mut added: Vec<&DefinedName> = Vec::new();
    for d in names {
        let lower = d.name.to_lowercase();
        let named = |key: Option<Option<usize>>| {
            seen.iter()
                .any(|(n, k)| *n == lower && (key.is_none() || k.is_none() || *k == key))
        };
        let missing = match d.scope {
            None => !named(None),
            Some(i) => aligned && i < sheet_count && !named(Some(Some(i))),
        };
        let filter_db = d.name.eq_ignore_ascii_case("_xlnm._FilterDatabase");
        if missing
            && !d.formula.is_empty()
            && (!filter_db || d.scope.is_some_and(|i| filtered.get(i) == Some(&true)))
            && !added
                .iter()
                .any(|a| a.scope == d.scope && a.name.eq_ignore_ascii_case(&d.name))
        {
            added.push(d);
        }
    }
    if !added.is_empty() {
        let element_prefix = names_close
            .as_ref()
            .map(|c| c.2.clone())
            .or_else(|| names_slot.as_ref().map(|s| s.1.clone()));
        if let Some(pfx) = element_prefix {
            let mut block = String::new();
            for d in &added {
                let mut scope = d
                    .scope
                    .map(|i| format!(" localSheetId=\"{i}\""))
                    .unwrap_or_default();
                if d.name.eq_ignore_ascii_case("_xlnm._FilterDatabase") {
                    scope.push_str(" hidden=\"1\"");
                }
                block.push_str(&format!(
                    "<{pfx}definedName name=\"{}\"{scope}>{}</{pfx}definedName>",
                    esc_attr(&d.name),
                    esc_text(&file_formula(&d.formula))
                ));
            }
            let (start, end, wrap) = match (names_close, names_slot) {
                (Some((s, e, _, self_closing)), _) => (s, e, self_closing),
                (None, Some((s, _))) => (s, s, true),
                (None, None) => unreachable!("a prefix came from one of them"),
            };
            if wrap {
                block = format!("<{pfx}definedNames>{block}</{pfx}definedNames>");
            }
            edits.push((start, end, block));
        }
    }
    edits.sort_by_key(|e| e.0);
    let mut out = xml.to_string();
    for (start, end, text) in edits.into_iter().rev() {
        out.replace_range(start..end, &text);
    }
    out
}

/// Where a `<definedName>` element starts, and how many sheets the model
/// has, for [`patch_defined_name`].
struct NameCtx {
    start: usize,
    sheet_count: usize,
}

/// The built-in names a save deletes once the model has none of them for a
/// sheet: Clear Print Area, clearing the print titles, and turning a filter
/// off.
fn removable_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("_xlnm.Print_Area")
        || name.eq_ignore_ascii_case("_xlnm.Print_Titles")
        || name.eq_ignore_ascii_case("_xlnm._FilterDatabase")
}

/// Queue the new content of the `<definedName>` whose start tag the parser is
/// on (not a self-closing one) where the model's definition differs, and
/// leave the parser past its end tag. `false` when the part ends first.
///
/// A `_xlnm.Print_Area`, `_xlnm.Print_Titles` or `_xlnm._FilterDatabase`
/// element whose (name, scope) the model no longer has is queued for removal
/// instead: Clear Print Area, clearing the titles and turning a filter off
/// delete the name. Only for a scoped key naming a
/// model sheet (`scope < sheet_count`), which `key` already is only while the
/// sheets line up; any other name the model lacks stays.
fn patch_defined_name(
    xml: &str,
    p: &mut XmlParser,
    key: Option<(String, Option<usize>)>,
    names: &[DefinedName],
    ctx: NameCtx,
    edits: &mut Vec<(usize, usize, String)>,
) -> bool {
    let body_start = p.pos();
    let mut text = String::new();
    let mut depth = 0usize;
    let mut nested = false;
    let body_end = loop {
        match p.next() {
            Event::Text => XmlParser::append_decoded(p.text(), &mut text),
            Event::Start => {
                depth += 1;
                nested = true;
            }
            Event::End if depth > 0 => depth -= 1,
            Event::End => break xml[..p.pos()].rfind("</"),
            Event::Eof => break None,
        }
    };
    let Some(body_end) = body_end else {
        return false;
    };
    let key = key.filter(|_| !nested);
    // A print area or titles the model no longer has, for a sheet the key
    // names reliably (it is only scoped while the sheets line up), was
    // cleared: its element goes.
    if let Some((name, Some(scope))) = &key {
        if removable_name(name)
            && *scope < ctx.sheet_count
            && !names
                .iter()
                .any(|d| d.scope == Some(*scope) && d.name.eq_ignore_ascii_case(name))
        {
            edits.push((ctx.start, p.pos(), String::new()));
            return true;
        }
    }
    let model = key.and_then(|(name, scope)| {
        let mut hits = names
            .iter()
            .filter(|d| d.scope == scope && d.name.eq_ignore_ascii_case(&name));
        match (hits.next(), hits.next()) {
            (Some(d), None) => Some(d),
            _ => None,
        }
    });
    // Compared in file spelling, so a loaded definition the model holds as it
    // was read is left alone.
    if let Some(f) = model
        .map(|d| file_formula(&d.formula))
        .filter(|f| *f != text)
    {
        edits.push((body_start, body_end, esc_text(&f)));
    }
    true
}

/// The (name, model scope) of the `<definedName>` start tag the parser is on;
/// `None` for a scope that doesn't name a model sheet reliably (see
/// [`patch_defined_names`]).
fn defined_name_key(p: &XmlParser, aligned: bool) -> Option<(String, Option<usize>)> {
    let scope = match p.attr("localSheetId") {
        "" => None,
        _ if !aligned => return None,
        v => Some(v.parse::<usize>().ok()?),
    };
    Some((decode(p.attr("name")), scope))
}

pub(crate) fn esc_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // Attribute-value normalization turns any literal whitespace into a
            // space, so tabs and newlines have to go in as entities too.
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            c if !xml_writable(c) => {}
            _ => out.push(ch),
        }
    }
    out
}

/// Escape an attribute value we captured VERBATIM from the source file, for
/// re-emission inside double quotes. Such a value is still encoded - it may
/// legally contain `"` and `>` because it came from a single-quoted attribute -
/// so `&` must be left exactly as it is or existing entities would be doubled.
pub(crate) fn esc_raw_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c if !xml_writable(c) => {}
            _ => out.push(ch),
        }
    }
    out
}

/// The chart kinds `chart_space_xml` can author. Everything else — scatter,
/// area, doughnut, radar, bubble, surface — falls into its `_` arm and would be
/// written back out as a clustered COLUMN chart. Worse, `parse_chart` reads
/// point DATA from `<c:cat>`/`<c:val>` only: a scatter's or bubble's
/// `<c:xVal>`/`<c:yVal>`/`<c:bubbleSize>` contribute their REFS (the box, and
/// `ChartSeries::point_refs`) but no cached numbers, so such a series carries
/// no values at all and regenerating one turns it into an empty column chart,
/// irreversibly. Those parts round-trip verbatim instead,
/// which is what they did before charts became editable — a stale ref beats a
/// destroyed chart.
///
/// This is a question about a chart's KIND, so the panel's type buttons could
/// walk straight through it — picking `column` for a scatter makes
/// [`chart_is_writable`] true and hands the writer exactly the series with no
/// values described above. They don't: `chart_set_kind` sends any chart holding
/// such a series through `chart_reauthored`, which re-derives it from its own
/// box first, so every series that reaches this gate as `column` really does
/// carry values.
pub fn chart_kind_is_writable(kind: &str) -> bool {
    matches!(kind, "bar" | "column" | "line" | "pie")
}

/// Whether an edit to this chart can be written back into its part at all: a
/// kind the writer can author, and a plot area it can reproduce. See
/// [`crate::sheet::ChartData::complex`] for the second half.
pub fn chart_is_writable(cd: &crate::sheet::ChartData) -> bool {
    chart_kind_is_writable(&cd.kind) && !cd.complex
}

/// [`chart_space_xml_in`] in the transitional namespaces. `pub(crate)` so
/// sibling modules' tests can round-trip parse → write → parse.
#[cfg(test)]
pub(crate) fn chart_space_xml(data: &crate::sheet::ChartData) -> String {
    chart_space_xml_in(data, &TRANSITIONAL)
}

/// A self-contained `chartSpace` for the kinds `chart_kind_is_writable`
/// accepts (bar, column, line, pie), in the namespaces of `ns`'s conformance
/// class. Each series and the categories are written as a `strRef`/`numRef`
/// naming the cells they read plus a cache of what those cells said, so the
/// chart stays live in Excel; a series with no reference falls back to
/// literals (`strLit`/`numLit`) and renders without a source range.
fn chart_space_xml_in(data: &crate::sheet::ChartData, ns: &OoxmlNs) -> String {
    // Each cache is sized from ITS OWN slot, never from a chart-wide maximum:
    // series can be re-pointed one at a time, so a 3-cell `<c:f>` beside a
    // 9-point cache is both invalid and self-contradicting — and `parse_chart`
    // reads the cache, so the six padding zeros would come back as real points.
    let ncat = data.categories.len();
    let cat_pts: String = data
        .categories
        .iter()
        .enumerate()
        .map(|(i, c)| format!("<c:pt idx=\"{i}\"><c:v>{}</c:v></c:pt>", esc_attr(c)))
        .collect();
    let ser_xml = |si: usize, s: &crate::sheet::ChartSeries| -> String {
        let nval = s.values.len();
        let val_pts: String = s
            .values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                // `<c:v>` is an xsd:double too — same `NaN`/`inf` hazard as a
                // worksheet cell's `<v>`, and the same answer.
                format!("<c:pt idx=\"{i}\"><c:v>{}</c:v></c:pt>", num_repr(*v))
            })
            .collect();
        // A range-backed chart writes the cells it reads alongside the cached
        // values, so Excel keeps it live; a snapshot writes the caches alone as
        // literals. The numbers are the same either way.
        let src = data.source.as_ref();
        // A series' own refs win; otherwise they are derived from the chart's
        // box and the column this series reads. NOT the name, though: a series
        // that reads cells but was NAMED by hand has `name_ref: None` on
        // purpose, and deriving its header cell would overwrite the typed name
        // with whatever that cell says the next time Excel refreshes.
        // `chart_from_range` sets `name_ref` itself, so nothing that wants a
        // live name loses one.
        let name_ref = s.name_ref.clone();
        let val_ref = s
            .values_ref
            .as_ref()
            .map(|v| v.to_ref())
            .or_else(|| Some(src?.f_ref(s.col?, s.col?, true)));
        // The categories' own ref wins in either orientation — `to_ref` writes
        // whatever rectangle it is given, so a label ROW comes out as
        // `$B$1:$D$1` with no help from here. Everything below it is the
        // COLUMN-shaped fallback for a chart that carries no `categories_ref`.
        let cat_ref = data
            .categories_ref
            .as_ref()
            .map(|v| v.to_ref())
            .or_else(|| {
                // Both halves of the fallback ask a column question, and a row
                // chart has no column answer. `cat_col` there is the column the
                // SERIES NAMES come from (`ChartSource::cat_col`), so `f_ref`
                // would hand Excel that column of names as the category labels
                // — or, on an imported chart whose first parsed ref was a values
                // ref, a column of plotted numbers; and `claimed_col` ("has a series already taken this
                // column?") is meaningless when every series spans the whole
                // width — it would answer yes for every column in the box, on
                // a chart where that says nothing about the labels. So the
                // fallback does not run: a row chart with no `categories_ref`
                // writes its labels as literals, the same answer the
                // all-numeric table gets.
                if data.by_row {
                    return None;
                }
                // Derive the category ref only from a box that HAS a label
                // column to spare, and only when no series has already claimed
                // that column. `cat_col` comes from whichever ref was read
                // first — for a chart whose categories are `<c:strLit>` that is
                // a series' own NAME or VALUES ref, and `<c:cat>` would then
                // name cells the labels never came from: Excel refreshes from
                // the ref it is given, so the user's typed labels would be
                // replaced by whatever those cells hold.
                let claimed_col = |c: u32| {
                    data.series.iter().any(|s| {
                        s.col == Some(c)
                            || s.values_ref
                                .as_ref()
                                .is_some_and(|v| v.range.1 <= c && c <= v.range.3)
                            || s.name_ref
                                .as_deref()
                                .and_then(crate::sheet::ChartSource::parse_f_ref)
                                .is_some_and(|v| v.range.1 <= c && c <= v.range.3)
                    })
                };
                src.filter(|sc| sc.range.1 != sc.range.3 && !claimed_col(sc.cat_col))
                    .map(|sc| sc.f_ref(sc.cat_col, sc.cat_col, true))
            });
        let name = match name_ref {
            Some(r) => format!(
                "<c:tx><c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>{}</c:v></c:pt></c:strCache></c:strRef></c:tx>",
                esc_attr(&r),
                esc_attr(&s.name)
            ),
            None => format!("<c:tx><c:v>{}</c:v></c:tx>", esc_attr(&s.name)),
        };
        let cat = match cat_ref {
            Some(r) => format!(
                "<c:cat><c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"{ncat}\"/>{cat_pts}</c:strCache></c:strRef></c:cat>",
                esc_attr(&r)
            ),
            // No reference AND no labels is not "labels that are all blank" —
            // it is a chart Excel numbers 1, 2, 3 itself. Writing an empty
            // `<c:strLit>` would blank the axis instead.
            None if ncat == 0 => String::new(),
            None => {
                format!("<c:cat><c:strLit><c:ptCount val=\"{ncat}\"/>{cat_pts}</c:strLit></c:cat>")
            }
        };
        let val = match val_ref {
            Some(r) => format!(
                "<c:val><c:numRef><c:f>{}</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"{nval}\"/>{val_pts}</c:numCache></c:numRef></c:val>",
                esc_attr(&r)
            ),
            None => format!(
                "<c:val><c:numLit><c:formatCode>General</c:formatCode><c:ptCount val=\"{nval}\"/>{val_pts}</c:numLit></c:val>"
            ),
        };
        // Where a series' colour lives depends on what is drawn: a line takes
        // it from its STROKE, everything else from its fill. Writing a fill for
        // a line would leave the line itself in Excel's default palette and
        // apply the colour to the shape instead — which is what the loader's
        // own `a_line_series_takes_its_colour_from_its_own_stroke` describes.
        let fill = match s.color {
            Some(rgb) if data.kind == "line" => format!(
                "<c:spPr><a:ln><a:solidFill><a:srgbClr val=\"{rgb:06X}\"/></a:solidFill></a:ln></c:spPr>"
            ),
            Some(rgb) => format!(
                "<c:spPr><a:solidFill><a:srgbClr val=\"{rgb:06X}\"/></a:solidFill></c:spPr>"
            ),
            None => String::new(),
        };
        format!("<c:ser><c:idx val=\"{si}\"/><c:order val=\"{si}\"/>{name}{fill}{cat}{val}</c:ser>")
    };
    let sers: String = data
        .series
        .iter()
        .enumerate()
        .map(|(si, s)| ser_xml(si, s))
        .collect();

    // catAx + valAx, shared by the axed chart types (bar/column/line). Pie omits them.
    const AXES: &str = "<c:catAx><c:axId val=\"111111111\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"b\"/><c:crossAx val=\"222222222\"/></c:catAx>\
<c:valAx><c:axId val=\"222222222\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"l\"/><c:crossAx val=\"111111111\"/></c:valAx>";
    const AX_IDS: &str = "<c:axId val=\"111111111\"/><c:axId val=\"222222222\"/>";

    let (plot_body, axes): (String, &str) = match data.kind.as_str() {
        "bar" => (
            format!(
                "<c:barChart><c:barDir val=\"bar\"/><c:grouping val=\"clustered\"/><c:varyColors val=\"0\"/>{sers}{AX_IDS}</c:barChart>"
            ),
            AXES,
        ),
        "line" => (
            format!(
                "<c:lineChart><c:grouping val=\"standard\"/><c:varyColors val=\"0\"/>{sers}<c:marker val=\"1\"/>{AX_IDS}</c:lineChart>"
            ),
            AXES,
        ),
        "pie" => {
            // ECMA-376 Part 1, DrawingML Charts (dml-chart.xsd): CT_PieChart's
            // content comes from EG_PieChartShared, which declares
            //   <xsd:element name="ser" type="CT_PieSer" minOccurs="0" maxOccurs="unbounded"/>
            // — so SEVERAL `<c:ser>` in one `<c:pieChart>` is schema-valid, and
            // real Excel-authored files do it (corpus: openoffice/.../pvt/
            // complex_29s.xlsx chart3.xml holds seven, libreoffice/.../
            // tdf111173.xlsx two). Excel plots the FIRST series only; that is a
            // plotting rule, not a format rule.
            // So EVERY series is written, the same `{sers}` the axed arms use.
            // This arm used to emit `series.first()` alone, on the claim that
            // "extra series are invalid (that's doughnut)" — the schema above
            // says otherwise, and dropping them deleted the user's work on
            // save. Keeping them costs nothing: Excel still plots the first,
            // and a chart converted to Pie and back keeps what it had.
            (
                format!(
                    "<c:pieChart><c:varyColors val=\"1\"/>{sers}<c:firstSliceAng val=\"0\"/></c:pieChart>"
                ),
                "",
            )
        }
        _ => (
            format!(
                "<c:barChart><c:barDir val=\"col\"/><c:grouping val=\"clustered\"/><c:varyColors val=\"0\"/>{sers}{AX_IDS}</c:barChart>"
            ),
            AXES,
        ),
    };

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<c:chartSpace xmlns:c=\"{}\" xmlns:a=\"{}\" xmlns:r=\"{}\">\
<c:chart><c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>{}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val=\"0\"/></c:title>\
<c:autoTitleDeleted val=\"0\"/><c:plotArea><c:layout/>\
{plot_body}{axes}\
</c:plotArea><c:legend><c:legendPos val=\"b\"/><c:overlay val=\"0\"/></c:legend><c:plotVisOnly val=\"1\"/><c:dispBlanksAs val=\"gap\"/></c:chart></c:chartSpace>",
        ns.chart,
        ns.dml,
        ns.rels,
        esc_attr(&data.title)
    )
}

/// Replace the `ref="…"` attribute value of the first `prefix` element.
/// Ensure `refreshOnLoad="1"` on the pivotCacheDefinition root element.
/// Idempotent, so a second save stays byte-identical.
fn set_refresh_on_load(xml: &str) -> String {
    let Some(start) = xml.find("<pivotCacheDefinition") else {
        return xml.to_string();
    };
    let Some(end) = xml[start..].find('>').map(|i| start + i) else {
        return xml.to_string();
    };
    let tag = &xml[start..end];
    if let Some(rel) = tag.find("refreshOnLoad=\"") {
        let vs = start + rel + "refreshOnLoad=\"".len();
        let Some(ve) = xml[vs..].find('"').map(|i| vs + i) else {
            return xml.to_string();
        };
        let mut out = xml.to_string();
        out.replace_range(vs..ve, "1");
        out
    } else {
        let mut out = xml.to_string();
        out.insert_str(
            start + "<pivotCacheDefinition".len(),
            " refreshOnLoad=\"1\"",
        );
        out
    }
}

/// The `<table>` root of a table part: its start position and an attribute's
/// decoded value (empty when it has none).
fn table_root_attr(xml: &str, attr: &str) -> Option<(usize, String)> {
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => {
                let value = p
                    .attrs()
                    .iter()
                    .find(|a| local(a.name) == attr)
                    .map(|a| decode(a.value))
                    .unwrap_or_default();
                return Some((p.start_pos(), value));
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// The name formulas knew a table part by when it was loaded (or written by
/// [`SheetPackage::add_table`]): its `displayName`, else its `name`. Save
/// never changes the package's own parts, so this is still the loaded name
/// after any number of renames and saves.
fn table_part_name(xml: &str) -> Option<String> {
    let (_, display) = table_root_attr(xml, "displayName")?;
    if !display.is_empty() {
        return Some(display);
    }
    table_root_attr(xml, "name").map(|(_, n)| n)
}

/// Every child element of the element spanning `xml[span]` whose local name
/// is `name`: (start, end) in `xml`.
fn child_spans(xml: &str, span: (usize, usize), name: &str) -> Vec<(usize, usize)> {
    element_children(&xml[span.0..span.1])
        .into_iter()
        .filter(|(n, _, _)| n == name)
        .map(|(_, s, e)| (span.0 + s, span.0 + e))
        .collect()
}

/// The table part's `<table>` children named `name`: (start, end).
fn table_children(xml: &str, name: &str) -> Vec<(usize, usize)> {
    let Some((root, _)) = table_root_attr(xml, "ref") else {
        return Vec::new();
    };
    let mut p = XmlParser::new(&xml[root..]);
    p.next();
    if !p.skip_element_complete() {
        return Vec::new();
    }
    child_spans(xml, (root, root + p.pos()), name)
}

/// A table converted to a range, or deleted with all its columns, whose part
/// a save drops: the name other table parts' formulas know it by, and where
/// its cells are (a deleted table's went with its delete, so those formulas
/// go `#REF!`).
struct ConvertedTable {
    part: String,
    loaded_name: String,
    edits: Vec<crate::formula::EditShift>,
    sheet: usize,
    sheet_name: String,
    info: crate::formula::TableInfo,
}

/// A table's columns renamed since its part was loaded: the name formulas in
/// the parts know the table by, and its columns' (old, new) names.
struct ColumnRenames {
    part: String,
    loaded_name: String,
    map: Vec<(String, String)>,
}

/// The formulas a table part holds in its columns (`calculatedColumnFormula`,
/// `totalsRowFormula`) with table columns renamed (`columns`: qualified
/// references in every part, unqualified ones in the table's own), tables
/// renamed (`renames`, old → new, applied at once) and converted tables'
/// references turned into cells. `own_sheet` and `own_part` are the sheet
/// and part of the table whose part this is. A formula that doesn't parse,
/// or that nothing touches, keeps its text.
fn rewrite_column_formulas(
    xml: &str,
    own_sheet: usize,
    own_part: &str,
    columns: &[ColumnRenames],
    renames: &[(String, String)],
    converted: &[ConvertedTable],
) -> String {
    let mut edits = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start
                if matches!(
                    local(p.name()),
                    "calculatedColumnFormula" | "totalsRowFormula"
                ) =>
            {
                let start = p.start_pos();
                let body = tag_end(xml, start);
                if !p.skip_element_complete() || xml[..body].ends_with("/>") {
                    continue;
                }
                let end = p.pos();
                let Some(close) = xml[..end].rfind("</").filter(|&c| c >= body) else {
                    continue;
                };
                let Ok(ast) = crate::formula::parse(&decode(&xml[body..close])) else {
                    continue;
                };
                let mut out = ast.clone();
                for c in converted {
                    let target = crate::formula::TableToRange {
                        name: &c.loaded_name,
                        sheet_name: &c.sheet_name,
                        info: &c.info,
                    };
                    let host = crate::formula::FormulaHost {
                        same_sheet: c.sheet == own_sheet,
                        row: None,
                        inside: false,
                    };
                    // Converted as the cells were, then moved through the
                    // edits since, by the rewrite that moved the cells.
                    out = crate::formula::map_expr(&out, &|x| {
                        if !matches!(
                            x,
                            crate::formula::Expr::Structured { .. } | crate::formula::Expr::Name(_)
                        ) {
                            return None;
                        }
                        let cells = crate::formula::table_refs_to_cells_in_expr(x, &target, host);
                        (cells != *x).then(|| {
                            c.edits.iter().fold(cells, |e, shift| {
                                let home = host.same_sheet;
                                crate::formula::adjust_for_edit(&e, home, &c.sheet_name, shift)
                            })
                        })
                    });
                }
                // By the names the file knows, before the tables' renames.
                for cr in columns {
                    let inside = cr.part == own_part;
                    out = crate::formula::rename_table_columns_in_expr(
                        &out,
                        &cr.loaded_name,
                        inside,
                        &cr.map,
                    );
                }
                out = crate::formula::rename_tables_in_expr(&out, renames);
                if out != ast {
                    // The file's spelling: `[#This Row]`, `_xlfn.` prefixes.
                    let text = crate::formula::to_file_string(&out);
                    edits.push((body, close, esc_text(&text)));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    apply_edits(xml.to_string(), edits)
}

/// Bring a table part's `<tableColumns>` to `columns`: a column the part
/// already has keeps its element, id and children (its `name` set when it
/// was renamed); a new one gets the next free id; a dropped one goes. With
/// `ids` (the model's `tableColumn id` per column, see
/// [`crate::sheet::Table::column_ids`]) columns are found by id alone, so a
/// renamed column keeps its element and a new column named like a dropped
/// one gets a new element; without, by name. The autoFilter's
/// `filterColumn`s follow their columns (`colId` is a column index) or go
/// with them. Also returns the ids of the elements dropped.
fn sync_table_columns(xml: &str, columns: &[String], ids: &[u32]) -> (String, Vec<u32>) {
    let Some(&span) = table_children(xml, "tableColumns").first() else {
        return (xml.to_string(), Vec::new());
    };
    let elements: Vec<(String, u32, &str)> = child_spans(xml, span, "tableColumn")
        .into_iter()
        .map(|(s, e)| {
            let tag = &xml[s..tag_end(xml, s)];
            let name = tag_attr(tag, "name").map(decode).unwrap_or_default();
            let id = tag_attr(tag, "id")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            (name, id, &xml[s..e])
        })
        .collect();
    let by_id = !ids.is_empty() && ids.len() == columns.len();
    let mut used = vec![false; elements.len()];
    // The element each column keeps, if any.
    let found: Vec<Option<usize>> = columns
        .iter()
        .enumerate()
        .map(|(j, name)| {
            let i = if by_id {
                (0..elements.len()).find(|&i| !used[i] && ids[j] != 0 && elements[i].1 == ids[j])
            } else {
                (0..elements.len())
                    .find(|&i| !used[i] && elements[i].0 == *name)
                    .or_else(|| {
                        (0..elements.len())
                            .find(|&i| !used[i] && elements[i].0.eq_ignore_ascii_case(name))
                    })
            };
            if let Some(i) = i {
                used[i] = true;
            }
            i
        })
        .collect();
    let unchanged = elements.len() == columns.len()
        && found
            .iter()
            .enumerate()
            .all(|(j, &i)| i == Some(j) && elements[j].0 == columns[j]);
    if unchanged {
        return (xml.to_string(), Vec::new());
    }
    let dropped: Vec<u32> = (elements.iter().zip(&used))
        .filter(|&(e, &kept)| !kept && e.1 != 0)
        .map(|(e, _)| e.1)
        .collect();
    let open = &xml[span.0..tag_end(xml, span.0)];
    let qname = open[1..]
        .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .next()
        .unwrap_or("tableColumns");
    let prefix = qname
        .rsplit_once(':')
        .map_or(String::new(), |(p, _)| format!("{p}:"));
    let mut next_id = elements.iter().map(|(_, id, _)| *id).max().unwrap_or(0);
    let mut body = String::new();
    for (name, &i) in columns.iter().zip(&found) {
        match i {
            Some(i) if elements[i].0 == *name => body.push_str(elements[i].2),
            Some(i) => body.push_str(&set_tag_attr(
                elements[i].2,
                0,
                "name",
                Some(&esc_attr(name)),
            )),
            None => {
                next_id += 1;
                body.push_str(&format!(
                    "<{prefix}tableColumn id=\"{next_id}\" name=\"{}\"/>",
                    esc_attr(name)
                ));
            }
        }
    }
    let block = format!("<{qname} count=\"{}\">{body}</{qname}>", columns.len());
    let mut edits = vec![(span.0, span.1, block)];
    // A filter on column k of the old columns moves to that column's new
    // index, or goes when the column did.
    if let Some(&af) = table_children(xml, "autoFilter").first() {
        for (s, e) in child_spans(xml, af, "filterColumn") {
            let tag_end_at = tag_end(xml, s);
            let col_id =
                tag_attr(&xml[s..tag_end_at], "colId").and_then(|v| v.parse::<usize>().ok());
            let moved = col_id.and_then(|k| found.iter().position(|&i| i == Some(k)));
            match moved {
                Some(k) if Some(k) == col_id => {}
                Some(k) => {
                    let el = set_tag_attr(&xml[s..e], 0, "colId", Some(&k.to_string()));
                    edits.push((s, e, el));
                }
                None => edits.push((s, e, String::new())),
            }
        }
    }
    (apply_edits(xml.to_string(), edits), dropped)
}

/// `xml` without any `<sortState>` (the table's own or its autoFilter's):
/// its refs name the old range, and Excel repairs a file whose sort state
/// lies outside its table.
fn drop_sort_state(xml: &str) -> String {
    let mut out = xml.to_string();
    while let Some((s, e)) = element_span_by_tags(&out, "sortState") {
        out.replace_range(s..e, "");
    }
    out
}

/// Remove a converted table's part from the package: the worksheet
/// relationship naming it, that worksheet's `<tablePart>` (and `<tableParts>`
/// when it was the last), then the part itself with its content type.
fn drop_table_part(parts: &mut Vec<(String, Vec<u8>)>, part: &str) {
    let mut owner: Option<(String, String)> = None;
    for (name, bytes) in parts.iter_mut() {
        let Some((dir, file)) = name
            .rsplit_once("/_rels/")
            .and_then(|(d, f)| Some((d.to_string(), f.strip_suffix(".rels")?.to_string())))
        else {
            continue;
        };
        let mut xml = String::from_utf8_lossy(bytes).into_owned();
        let hit = parse_rels(&xml)
            .into_iter()
            .find(|(_, ty, t)| ty.ends_with("/table") && resolve_relative(&dir, t) == part);
        let Some((id, _, _)) = hit else {
            continue;
        };
        if let Some(el) = find_element_by_attr(&xml, "Relationship", "Id", |v| v == id) {
            xml.replace_range(el.start..el.end, "");
        }
        *bytes = xml.into_bytes();
        owner = Some((format!("{dir}/{file}"), id));
        break;
    }
    if let Some((ws, id)) = owner {
        if let Some(p) = parts.iter_mut().find(|(n, _)| *n == ws) {
            let mut xml = String::from_utf8_lossy(&p.1).into_owned();
            if let Some(el) = find_element_by_attr(&xml, "tablePart", "id", |v| v == id) {
                xml.replace_range(el.start..el.end, "");
                if let Some((s, e)) = worksheet_child_span(&xml, "tableParts") {
                    let left = element_children(&xml[s..e])
                        .iter()
                        .filter(|(n, _, _)| n == "tablePart")
                        .count();
                    xml = if left == 0 {
                        remove_worksheet_child(&xml, "tableParts")
                    } else {
                        set_tag_attr(&xml, s, "count", Some(&left.to_string()))
                    };
                }
            }
            p.1 = xml.into_bytes();
        }
    }
    drop_parts_cascading(parts, part);
}

/// Table `t`'s columns by the names its part (`xml`) knows them by: a column
/// renamed since the part was written ([`crate::sheet::Table::column_ids`])
/// has the part's name for it, every other column its own.
fn part_column_names(t: &Table, xml: &str) -> Vec<String> {
    let loaded = parse_table_xml(xml, t.sheet, &t.part);
    let part_name = |j: usize| {
        let id = *t.column_ids.get(j).filter(|&&id| id != 0)?;
        let loaded = loaded.as_ref()?;
        let k = loaded.column_ids.iter().position(|&x| x == id)?;
        loaded.columns.get(k).cloned()
    };
    (t.columns.iter().enumerate())
        .map(|(j, name)| part_name(j).unwrap_or_else(|| name.clone()))
        .collect()
}

/// Bring the table parts in line with the model: each table's range, name
/// and columns (renamed ones by id, deleted ones dropped), the column
/// formulas of every part (renamed tables and columns, converted tables),
/// and the parts of tables converted to a range or deleted with all their
/// columns dropped.
fn sync_table_parts(parts: &mut Vec<(String, Vec<u8>)>, wb: &Workbook) {
    let part_xml = |parts: &[(String, Vec<u8>)], name: &str| {
        parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
    };
    let renames: Vec<(String, String)> = wb
        .tables
        .iter()
        .filter_map(|t| {
            let loaded = table_part_name(&part_xml(parts, &t.part)?)?;
            (loaded != t.name).then(|| (loaded, t.name.clone()))
        })
        .collect();
    // A column keeps its id through a rename (`Table::column_ids`): the
    // part's element with that id still has the old name.
    let column_renames: Vec<ColumnRenames> = wb
        .tables
        .iter()
        .filter_map(|t| {
            let xml = part_xml(parts, &t.part)?;
            let map: Vec<(String, String)> = part_column_names(t, &xml)
                .into_iter()
                .zip(&t.columns)
                .filter(|(old, new)| old != *new)
                .map(|(old, new)| (old, new.clone()))
                .collect();
            let loaded_name = table_part_name(&xml)?;
            (!map.is_empty()).then(|| ColumnRenames {
                part: t.part.clone(),
                loaded_name,
                map,
            })
        })
        .collect();
    let converted: Vec<ConvertedTable> = wb
        .removed_tables
        .iter()
        .filter(|r| !wb.tables.iter().any(|t| t.part == r.table.part))
        .filter_map(|removed| {
            let r = &removed.table;
            let xml = part_xml(parts, &r.part)?;
            let loaded_name = table_part_name(&xml)?;
            // Other parts' formulas name its columns as its part does: a
            // column renamed before the conversion goes by its old name.
            let mut info = r.info();
            info.columns = part_column_names(r, &xml);
            Some(ConvertedTable {
                part: r.part.clone(),
                loaded_name,
                edits: removed.edits.clone(),
                sheet: r.sheet,
                sheet_name: wb.sheets.get(r.sheet)?.name.clone(),
                info,
            })
        })
        .collect();
    let mut dropped_columns: Vec<(String, Vec<u32>)> = Vec::new();
    for t in &wb.tables {
        let Some(p) = parts.iter_mut().find(|(n, _)| n == &t.part) else {
            continue;
        };
        let xml = String::from_utf8_lossy(&p.1).into_owned();
        let (r1, c1, r2, c2) = t.range;
        let full = format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2));
        let moved = table_root_attr(&xml, "ref").is_some_and(|(_, r)| r != full);
        let mut updated = patch_ref_attr(&xml, "<table", &full);
        // autoFilter covers the table minus its totals row.
        let af_r2 = r2.saturating_sub(t.totals_rows).max(r1);
        let af = format!("{}:{}", cell_name(r1, c1), cell_name(af_r2, c2));
        updated = patch_ref_attr(&updated, "<autoFilter", &af);
        if moved {
            updated = drop_sort_state(&updated);
        }
        if table_part_name(&updated).is_some_and(|n| n != t.name) {
            for attr in ["name", "displayName"] {
                if let Some((root, _)) = table_root_attr(&updated, attr) {
                    updated = set_tag_attr(&updated, root, attr, Some(&esc_attr(&t.name)));
                }
            }
        }
        let dropped;
        (updated, dropped) = sync_table_columns(&updated, &t.columns, &t.column_ids);
        if !dropped.is_empty() {
            dropped_columns.push((t.part.clone(), dropped));
        }
        if !column_renames.is_empty() || !renames.is_empty() || !converted.is_empty() {
            updated = rewrite_column_formulas(
                &updated,
                t.sheet,
                &t.part,
                &column_renames,
                &renames,
                &converted,
            );
        }
        updated = sync_calculated_formulas(&updated, &t.name, &t.calculated_formulas);
        p.1 = updated.into_bytes();
    }
    for (part, dropped) in &dropped_columns {
        drop_query_table_fields(parts, part, dropped);
    }
    for c in &converted {
        drop_table_part(parts, &c.part);
    }
}

/// A text event's content: CDATA as it stands, other text decoded.
fn push_text(p: &XmlParser, out: &mut String) {
    if p.is_cdata() {
        out.push_str(p.text());
    } else {
        XmlParser::append_decoded(p.text(), out);
    }
}

/// The text of the element `el` (one element, its own tags included), as
/// the loader reads it.
fn element_text(el: &str) -> String {
    let mut p = XmlParser::new(el);
    let mut out = String::new();
    loop {
        match p.next() {
            Event::Text => push_text(&p, &mut out),
            Event::Eof => return out,
            _ => {}
        }
    }
}

/// Give each `tableColumn` of a table part (in the model's column order, as
/// [`sync_table_columns`] left them) the model's calculated-column formula
/// ([`crate::sheet::Table::calculated_formulas`]). An element whose formula
/// already means the same (the same parse, after the rewrites the save
/// made) keeps its text, so an unchanged part stays as loaded. A column the
/// model holds no formula for loses the part's, unless that is an array
/// formula (which the model doesn't hold) or an empty element (which loads
/// as none); a model that knows no formulas
/// at all leaves the part's alone. A new formula is written as Excel writes
/// one, its bare references qualified by the table's name `table`
/// (`[@Qty]` is `Sales[[#This Row],[Qty]]`).
fn sync_calculated_formulas(xml: &str, table: &str, formulas: &[Option<String>]) -> String {
    if formulas.is_empty() {
        return xml.to_string();
    }
    let Some(&span) = table_children(xml, "tableColumns").first() else {
        return xml.to_string();
    };
    // A formula with its bare references naming `table`.
    let qualified = |src: &str| {
        crate::formula::parse(src)
            .ok()
            .map(|e| crate::formula::qualify_bare_refs(&e, table))
    };
    let mut edits = Vec::new();
    for (j, (s, e)) in child_spans(xml, span, "tableColumn")
        .into_iter()
        .enumerate()
    {
        let open_end = tag_end(xml, s);
        let open = &xml[s..open_end];
        // The part's own formula element, if any.
        let existing = (!open.ends_with("/>"))
            .then(|| {
                element_children(&xml[s..e])
                    .into_iter()
                    .find(|(n, _, _)| n == "calculatedColumnFormula")
                    .map(|(_, a, b)| (s + a, s + b))
            })
            .flatten();
        let Some(f) = formulas.get(j).and_then(Option::as_deref) else {
            // Only a formula the model cleared goes: an empty element (which
            // loads as none) stays as the file has it.
            if let Some((a, b)) = existing {
                let tag = &xml[a..tag_end(xml, a)];
                let array = matches!(tag_attr(tag, "array"), Some("1" | "true"));
                if !array && !element_text(&xml[a..b]).trim().is_empty() {
                    edits.push((a, b, String::new()));
                }
            }
            continue;
        };
        let want = qualified(f);
        let body = esc_text(&match &want {
            Some(e) => crate::formula::to_file_string(e),
            None => crate::formula::file_formula(f).into_owned(),
        });
        let qname = open[1..]
            .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .next()
            .unwrap_or("tableColumn");
        let prefix = qname
            .rsplit_once(':')
            .map_or(String::new(), |(p, _)| format!("{p}:"));
        let calc =
            format!("<{prefix}calculatedColumnFormula>{body}</{prefix}calculatedColumnFormula>");
        if let Some(head) = open.strip_suffix("/>") {
            let head = head.trim_end();
            edits.push((s, e, format!("{head}>{calc}</{qname}>")));
            continue;
        }
        match existing {
            Some((a, b)) => {
                let text = element_text(&xml[a..b]);
                let same = text == f || (want.is_some() && qualified(&text) == want);
                if !same {
                    edits.push((a, b, calc));
                }
            }
            None => edits.push((open_end, open_end, calc)),
        }
    }
    apply_edits(xml.to_string(), edits)
}

/// Table `part` lost the columns whose ids are `dropped`: the query table
/// behind it (`tableType="queryTable"`, reached through the part's
/// relationships) loses the `queryTableField`s bound to them, and its
/// `queryTableFields count` follows. Nothing else in that part changes; a
/// table with no query table, or one whose part can't be found, is left
/// alone.
fn drop_query_table_fields(parts: &mut [(String, Vec<u8>)], part: &str, dropped: &[u32]) {
    let Some((dir, _)) = part.rsplit_once('/') else {
        return;
    };
    let rels_name = rels_part_name(part);
    let Some((_, rels)) = parts.iter().find(|(n, _)| *n == rels_name) else {
        return;
    };
    let targets: Vec<String> = parse_rels(&String::from_utf8_lossy(rels))
        .into_iter()
        .filter(|(_, ty, _)| ty.ends_with("/queryTable"))
        .map(|(_, _, target)| resolve_relative(dir, &target))
        .collect();
    for target in targets {
        let Some(p) = parts.iter_mut().find(|(n, _)| *n == target) else {
            continue;
        };
        let xml = String::from_utf8_lossy(&p.1).into_owned();
        let Some(span) = element_span_by_tags(&xml, "queryTableFields") else {
            continue;
        };
        let fields = child_spans(&xml, span, "queryTableField");
        let gone: Vec<(usize, usize, String)> = fields
            .iter()
            .filter(|&&(s, _)| {
                tag_attr(&xml[s..tag_end(&xml, s)], "tableColumnId")
                    .and_then(|v| v.parse::<u32>().ok())
                    .is_some_and(|id| dropped.contains(&id))
            })
            .map(|&(s, e)| (s, e, String::new()))
            .collect();
        if gone.is_empty() {
            continue;
        }
        let left = (fields.len() - gone.len()).to_string();
        let xml = apply_edits(xml, gone);
        let Some((start, _)) = element_span_by_tags(&xml, "queryTableFields") else {
            continue;
        };
        let end = tag_end(&xml, start);
        let tag = set_tag_attr_in_place(xml[start..end].to_string(), "count", &left);
        p.1 = apply_edits(xml, vec![(start, end, tag)]).into_bytes();
    }
}

fn patch_ref_attr(xml: &str, prefix: &str, new_ref: &str) -> String {
    let Some(el) = xml.find(prefix) else {
        return xml.to_string();
    };
    let Some(rel) = xml[el..].find("ref=\"") else {
        return xml.to_string();
    };
    let vs = el + rel + 5;
    let Some(ve) = xml[vs..].find('"') else {
        return xml.to_string();
    };
    let mut out = xml.to_string();
    out.replace_range(vs..vs + ve, new_ref);
    out
}

/// Replace the `sheet="…"` attribute value of a pivot cache's
/// `<worksheetSource>` element — keeps the persisted cache in sync with
/// [`crate::pivot::PivotSource::Range`]'s `sheet` after it changes in the
/// model (namely a `rename_sheet` of the pivot's source sheet, which doesn't
/// set `edited` and so wouldn't otherwise touch this part). A no-op when the
/// element or attribute is missing — e.g. a `PivotSource::Table` cache's
/// `<worksheetSource name="…"/>` has no `sheet` attribute at all.
fn patch_worksheet_source_sheet(xml: &str, new_sheet: &str) -> String {
    let Some(el) = xml.find("<worksheetSource") else {
        return xml.to_string();
    };
    let Some(end) = xml[el..].find('>').map(|i| el + i) else {
        return xml.to_string();
    };
    let Some(rel) = xml[el..end].find("sheet=\"") else {
        return xml.to_string();
    };
    let vs = el + rel + "sheet=\"".len();
    let Some(ve) = xml[vs..].find('"') else {
        return xml.to_string();
    };
    let mut out = xml.to_string();
    out.replace_range(vs..vs + ve, &esc_attr(new_sheet));
    out
}

/// Point a pivot cache's `<worksheetSource name="…">` at table `name`; a
/// no-op for a range source (no `name`) or when it already does.
fn patch_worksheet_source_name(xml: &str, name: &str) -> String {
    let Some(el) = xml.find("<worksheetSource") else {
        return xml.to_string();
    };
    match attr_at(xml, el, "name") {
        Some(cur) if decode(cur) != name => set_tag_attr(xml, el, "name", Some(&esc_attr(name))),
        _ => xml.to_string(),
    }
}

/// Update count/uniqueCount attributes on `<sst …>`.
fn patch_counts(xml: &str, total: usize) -> String {
    let mut out = xml.to_string();
    for key in ["count=\"", "uniqueCount=\""] {
        if let Some(i) = out.find(key) {
            let vs = i + key.len();
            if let Some(ve) = out[vs..].find('"') {
                out.replace_range(vs..vs + ve, &total.to_string());
            }
        }
    }
    out
}

/// Remove the first `prefix…/>` element whose text contains `needle`.
///
/// A `prefix` ending in a name character matches only the whole element
/// name: `<Relationship` does not match the `<Relationships>` root, nor
/// `<pivotCache` the `<pivotCaches>` wrapper, whose span up to the first
/// child's `/>` would otherwise take the wrapper's open tag with it.
fn remove_element_containing(xml: &str, prefix: &str, needle: &str) -> String {
    let whole_name = prefix.bytes().last().is_some_and(is_xml_name_byte);
    let mut search_from = 0;
    while let Some(rel) = xml[search_from..].find(prefix) {
        let start = search_from + rel;
        if whole_name
            && xml
                .as_bytes()
                .get(start + prefix.len())
                .is_some_and(|&b| is_xml_name_byte(b))
        {
            search_from = start + prefix.len();
            continue;
        }
        let end = match xml[start..].find("/>") {
            Some(i) => start + i + 2,
            None => break,
        };
        if xml[start..end].contains(needle) {
            return format!("{}{}", &xml[..start], &xml[end..]);
        }
        search_from = end;
    }
    xml.to_string()
}

/// Whether `b` can continue an XML name (ASCII letters, digits, `-_.:`, and
/// any byte of a non-ASCII character).
fn is_xml_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':') || b >= 0x80
}

/// CT_Workbook's children, in the order the schema requires them.
const CT_WORKBOOK_ORDER: [&str; 19] = [
    "fileVersion",
    "fileSharing",
    "workbookPr",
    "workbookProtection",
    "bookViews",
    "sheets",
    "functionGroups",
    "externalReferences",
    "definedNames",
    "calcPr",
    "oleSize",
    "customWorkbookViews",
    "pivotCaches",
    "smartTagPr",
    "smartTagTypes",
    "webPublishing",
    "fileRecoveryPr",
    "webPublishObjects",
    "extLst",
];

/// Where a new top-level child goes in workbook.xml ([`workbook_slot`]).
#[derive(Debug, PartialEq)]
enum WorkbookSlot {
    /// Here, in the root's namespace prefix (`"x:"`, or `""`).
    At(usize, String),
    /// The workbook already has one, in some prefix: a caller that looked
    /// for it by one spelling only must not add a second.
    Present,
    /// The walk couldn't see every child (a truncated part, a self-closing
    /// root), so it can vouch for neither.
    Unknown,
}

/// Where a new top-level `<tag>` goes in workbook.xml: before the first
/// child the schema ranks after `tag`, else before the root's end tag.
/// Children the schema doesn't name (`mc:AlternateContent`, …) are not
/// anchors.
fn workbook_slot(xml: &str, tag: &str) -> WorkbookSlot {
    let rank_of = |name: &str| CT_WORKBOOK_ORDER.iter().position(|&t| t == name);
    let rank = rank_of(tag).unwrap_or(CT_WORKBOOK_ORDER.len());
    let mut p = XmlParser::new(xml);
    // The root start tag.
    loop {
        match p.next() {
            Event::Start => break,
            Event::Text => {}
            Event::End | Event::Eof => return WorkbookSlot::Unknown,
        }
    }
    let prefix = match p.name().rsplit_once(':') {
        Some((pfx, _)) => format!("{pfx}:"),
        None => String::new(),
    };
    let mut at = None;
    loop {
        match p.next() {
            Event::Start => {
                let name = local(p.name());
                if name == tag {
                    return WorkbookSlot::Present;
                }
                if at.is_none() && rank_of(name).is_some_and(|r| r > rank) {
                    at = Some(p.start_pos());
                }
                if !p.skip_element_complete() {
                    return WorkbookSlot::Unknown;
                }
            }
            Event::End => {
                // The root's end tag (a self-closing root has no `</`).
                return match xml[..p.pos()].rfind("</") {
                    Some(end) => WorkbookSlot::At(at.unwrap_or(end), prefix),
                    None => WorkbookSlot::Unknown,
                };
            }
            Event::Text => {}
            Event::Eof => return WorkbookSlot::Unknown,
        }
    }
}

/// A top-level child of workbook.xml, found by [`workbook_child`].
#[derive(Debug, PartialEq)]
struct WorkbookChild {
    /// The `<` of its start tag.
    start: usize,
    /// Just past its end tag (or its `/>`).
    end: usize,
    /// `<x:pivotCaches/>`: no content, and no end tag to insert before.
    self_closing: bool,
    /// Its name as written (`x:pivotCaches`).
    qname: String,
}

/// The top-level `<tag>` of workbook.xml in any prefix, read to its end.
/// `None` when there is none, or when the walk can't reach a complete one
/// (a truncated part, a self-closing root).
fn workbook_child(xml: &str, tag: &str) -> Option<WorkbookChild> {
    let mut p = XmlParser::new(xml);
    // The root start tag.
    loop {
        match p.next() {
            Event::Start => break,
            Event::Text => {}
            Event::End | Event::Eof => return None,
        }
    }
    loop {
        match p.next() {
            Event::Start => {
                let start = p.start_pos();
                let qname = p.name().to_string();
                let self_closing = xml[..p.pos()].ends_with("/>");
                if !p.skip_element_complete() {
                    return None;
                }
                if local(&qname) == tag {
                    return Some(WorkbookChild {
                        start,
                        end: p.pos(),
                        self_closing,
                        qname,
                    });
                }
            }
            Event::Text => {}
            Event::End | Event::Eof => return None,
        }
    }
}

/// Whether a top-level `<tag>` standing at `at` in workbook.xml is in its
/// CT_Workbook place: no child the schema ranks after it comes before `at`,
/// and none it ranks before comes after. Children the schema doesn't name
/// don't count. `false` when the walk can't see every child.
fn workbook_order_holds(xml: &str, at: usize, tag: &str) -> bool {
    let rank_of = |name: &str| CT_WORKBOOK_ORDER.iter().position(|&t| t == name);
    let Some(rank) = rank_of(tag) else {
        return false;
    };
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => break,
            Event::Text => {}
            Event::End | Event::Eof => return false,
        }
    }
    loop {
        match p.next() {
            Event::Start => {
                let before = p.start_pos() < at;
                let out_of_order = rank_of(local(p.name()))
                    .is_some_and(|r| if before { r > rank } else { r < rank });
                if out_of_order || !p.skip_element_complete() {
                    return false;
                }
            }
            Event::Text => {}
            Event::End => return true,
            Event::Eof => return false,
        }
    }
}

/// workbook.xml whose `<workbookPr date1904>` says `on`, in any prefix. An
/// existing workbookPr gets the attribute set to "1", or loses a true one;
/// a 1904 workbook without a workbookPr gets one at its schema position.
/// A part that already agrees is returned unchanged.
fn set_date1904(xml: &str, on: bool) -> String {
    match workbook_child(xml, "workbookPr") {
        Some(c) => {
            let was = matches!(attr_at(xml, c.start, "date1904"), Some("1" | "true"));
            if was == on {
                xml.to_string()
            } else {
                set_tag_attr(xml, c.start, "date1904", on.then_some("1"))
            }
        }
        None if !on => xml.to_string(),
        None => match workbook_slot(xml, "workbookPr") {
            WorkbookSlot::At(at, px) => {
                let mut out = xml.to_string();
                out.insert_str(at, &format!("<{px}workbookPr date1904=\"1\"/>"));
                out
            }
            // One the walk saw but couldn't read, or an unreadable part:
            // left as it is rather than given a second workbookPr.
            WorkbookSlot::Present | WorkbookSlot::Unknown => xml.to_string(),
        },
    }
}

/// Guarantee `<calcPr … fullCalcOnLoad="1"/>` in workbook.xml, in any prefix:
/// an existing calcPr gets the attribute (or has a `0`/`false` one turned on,
/// since save has just dropped the calc chain), and a workbook without one
/// gets one at its schema position.
fn ensure_full_calc(xml: &str) -> String {
    let existing = match workbook_child(xml, "calcPr") {
        Some(c) => Some(c.start),
        None => match workbook_slot(xml, "calcPr") {
            // At its schema position: after definedNames, before pivotCaches
            // and extLst, which a bare append would put it behind.
            WorkbookSlot::At(at, px) => {
                let mut out = xml.to_string();
                out.insert_str(
                    at,
                    &format!("<{px}calcPr calcId=\"0\" fullCalcOnLoad=\"1\"/>"),
                );
                return out;
            }
            // The walk couldn't read the part to its end: the first calcPr
            // its tags show, if any.
            WorkbookSlot::Present | WorkbookSlot::Unknown => {
                find_local_element(xml, "calcPr").map(|(i, _)| i)
            }
        },
    };
    let Some(start) = existing else {
        return xml.replacen(
            "</workbook>",
            "<calcPr calcId=\"0\" fullCalcOnLoad=\"1\"/></workbook>",
            1,
        );
    };
    let end = tag_end(xml, start);
    let tag = &xml[start..end];
    // A start tag the part cuts off (no `>`): no place to put the attribute.
    if !tag.ends_with('>') {
        return xml.to_string();
    }
    match attr_span(tag, "fullCalcOnLoad") {
        Some((_, s, e, _)) if matches!(&tag[s..e], "1" | "true") => xml.to_string(),
        Some((_, s, e, _)) => format!("{}1{}", &xml[..start + s], &xml[start + e..]),
        None => {
            // Before the tag's end, after the attributes already there.
            let at = if tag.ends_with("/>") {
                end - 2
            } else {
                end - 1
            };
            format!("{} fullCalcOnLoad=\"1\"{}", &xml[..at], &xml[at..])
        }
    }
}

/// workbook.xml with `<pivotCache cacheId=… r:id=…/>` registered. An existing
/// `<pivotCaches>` (any prefix, self-closing or not) takes the entry in its
/// own prefix, and moves to its CT_Workbook place when it stood out of it;
/// without one, a new wrapper goes at that place. The entry's `r:` is bound
/// to `rels`: on the root when the root binds no `r`, else on the entry
/// when the root binds it to something else.
fn register_pivot_cache(xml: &str, cache_id: u32, rid: &str, rels: &str) -> String {
    let root = worksheet_root(xml);
    let r_elsewhere = root
        .as_ref()
        .and_then(|r| r.r_ns.as_deref())
        .is_some_and(|r| r != rels);
    let entry = |px: &str| {
        let e = format!("<{px}pivotCache cacheId=\"{cache_id}\" r:id=\"{rid}\"/>");
        if r_elsewhere {
            with_decl(&e, &format!("xmlns:r=\"{rels}\""))
        } else {
            e
        }
    };
    let mut out = match workbook_child(xml, "pivotCaches") {
        Some(c) => {
            let px = match c.qname.rsplit_once(':') {
                Some((pfx, _)) => format!("{pfx}:"),
                None => String::new(),
            };
            let old = &xml[c.start..c.end];
            let wrapper = if c.self_closing {
                // `<x:pivotCaches/>`: open it up.
                format!("{}>{}</{}>", &old[..old.len() - 2], entry(&px), c.qname)
            } else {
                let close = old.rfind("</").unwrap_or(old.len());
                format!("{}{}{}", &old[..close], entry(&px), &old[close..])
            };
            let mut rest = format!("{}{}", &xml[..c.start], &xml[c.end..]);
            // Where it stands, unless that is out of CT_Workbook order (an
            // older docxy put it right after `</sheets>`): then where a new
            // one would go.
            let at = if workbook_order_holds(xml, c.start, "pivotCaches") {
                c.start
            } else {
                match workbook_slot(&rest, "pivotCaches") {
                    WorkbookSlot::At(at, _) => at,
                    WorkbookSlot::Present | WorkbookSlot::Unknown => c.start,
                }
            };
            rest.insert_str(at, &wrapper);
            rest
        }
        None => match workbook_slot(xml, "pivotCaches") {
            // At its CT_Workbook position (after definedNames, calcPr, …),
            // not right after `</sheets>`, which Excel repairs.
            WorkbookSlot::At(at, px) => {
                let mut out = xml.to_string();
                out.insert_str(
                    at,
                    &format!("<{px}pivotCaches>{}</{px}pivotCaches>", entry(&px)),
                );
                out
            }
            // One the walk saw but couldn't read to its end: not registered.
            WorkbookSlot::Present => xml.to_string(),
            // The walk couldn't read the part: the spelling docxy writes.
            WorkbookSlot::Unknown if xml.contains("</pivotCaches>") => {
                xml.replacen("</pivotCaches>", &format!("{}</pivotCaches>", entry("")), 1)
            }
            WorkbookSlot::Unknown => xml.replacen(
                "</sheets>",
                &format!("</sheets><pivotCaches>{}</pivotCaches>", entry("")),
                1,
            ),
        },
    };
    // Last: every edit above lies past the root's start tag, so its offsets
    // still hold here.
    if let Some(root) = root.filter(|r| r.r_ns.is_none() && !r.self_closing && out != xml) {
        out.insert_str(root.tag_close, &format!(" xmlns:r=\"{rels}\""));
    }
    out
}

/// workbook.xml without the `<pivotCache>` whose `r:id` is `rid`, undoing
/// [`register_pivot_cache`] in any prefix: the wrapper goes too when no
/// entry is left in it, since the schema wants at least one. A part the walk
/// can't read loses the entry by its unprefixed spelling, as before.
fn unregister_pivot_cache(xml: &str, rid: &str) -> String {
    let Some(c) = workbook_child(xml, "pivotCaches") else {
        let xml = remove_element_containing(xml, "<pivotCache", &format!("r:id=\"{rid}\""));
        return xml.replace("<pivotCaches></pivotCaches>", "");
    };
    let mut p = XmlParser::new(&xml[c.start..c.end]);
    p.next(); // the wrapper's start tag
    let mut entries = 0;
    let mut hit = None;
    loop {
        match p.next() {
            Event::Start => {
                let start = c.start + p.start_pos();
                let is_entry = local(p.name()) == "pivotCache";
                let is_hit = is_entry
                    && p.attrs()
                        .iter()
                        .any(|a| local(a.name) == "id" && a.value == rid);
                if !p.skip_element_complete() {
                    break;
                }
                if is_entry {
                    entries += 1;
                }
                if is_hit && hit.is_none() {
                    hit = Some((start, c.start + p.pos()));
                }
            }
            Event::Text => {}
            Event::End | Event::Eof => break,
        }
    }
    match hit {
        None => xml.to_string(),
        // The last entry: the wrapper goes with it.
        Some(_) if entries == 1 => format!("{}{}", &xml[..c.start], &xml[c.end..]),
        Some((s, e)) => format!("{}{}", &xml[..s], &xml[e..]),
    }
}

/// A self-closed root element (`<xdr:wsDr …/>`) reopened: everything up to and
/// including its `>`, with the `/` dropped, ready for children and a close tag.
/// `None` when `xml` holds no such element at all.
fn open_self_closed_root(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}");
    let at = xml.find(&open)?;
    let after = at + open.len();
    // `<xdr:wsDrSomething` merely starts the same way.
    if !xml[after..]
        .chars()
        .next()
        .is_some_and(|c| c.is_whitespace() || c == '/' || c == '>')
    {
        return None;
    }
    let gt = xml[after..].find('>')? + after;
    xml[..gt].strip_suffix('/').map(|b| format!("{b}>"))
}

pub(crate) fn add_content_type_override(
    parts: &mut [(String, Vec<u8>)],
    part_name: &str,
    ct: &str,
) {
    if let Some(p) = parts.iter_mut().find(|(n, _)| n == "[Content_Types].xml") {
        let xml = String::from_utf8_lossy(&p.1).into_owned();
        if xml.contains(part_name) {
            return;
        }
        let ov = format!("<Override PartName=\"{part_name}\" ContentType=\"{ct}\"/>");
        p.1 = xml
            .replacen("</Types>", &format!("{ov}</Types>"), 1)
            .into_bytes();
    }
}

/// One attribute of the first `open`-prefixed element in `xml`, read without a
/// full parse: `attr_of_tag(ws, "<drawing ", "r:id")`. Only looks inside that
/// one element, so a later tag carrying the same attribute can't answer for it.
fn attr_of_tag(xml: &str, open: &str, attr: &str) -> Option<String> {
    let start = xml.find(open)? + open.len();
    let tag = &xml[start..start + xml[start..].find('>')?];
    let at = tag.find(&format!("{attr}=\""))? + attr.len() + 2;
    let end = tag[at..].find('"')? + at;
    Some(tag[at..end].to_string())
}

/// The `Target` a relationship stored in `dir` needs in order to name `part`,
/// both given as package-root paths: `("xl/drawings", "xl/charts/chart1.xml")`
/// → `../charts/chart1.xml`. The inverse of [`resolve_relative`].
fn relative_target(dir: &str, part: &str) -> String {
    let d: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let p: Vec<&str> = part.split('/').filter(|s| !s.is_empty()).collect();
    let common = d.iter().zip(p.iter()).take_while(|(a, b)| a == b).count();
    let mut out = "../".repeat(d.len() - common);
    out.push_str(&p[common..].join("/"));
    out
}

/// The namespace prefix a drawing part's root element carries (`xdr:` for
/// anything Excel wrote; empty when the part declares the spreadsheet-drawing
/// namespace as its default), so an anchor spliced in matches it.
fn wsdr_prefix(xml: &str) -> &str {
    let Some(at) = xml.find("wsDr") else {
        return "xdr:";
    };
    let open = xml[..at].rfind('<').map(|i| i + 1).unwrap_or(at);
    &xml[open..at]
}

/// The rId a rels part already gives `target`, if any — what [`add_rel`]
/// declines to assign a second time.
fn rel_id_for(parts: &[(String, Vec<u8>)], rels_part: &str, target: &str) -> Option<String> {
    let xml = parts
        .iter()
        .find(|(n, _)| n == rels_part)
        .map(|(_, b)| String::from_utf8_lossy(b).into_owned())?;
    let at = xml.find(&format!("Target=\"{target}\""))?;
    let open = xml[..at].rfind("<Relationship")?;
    attr_of_tag(&xml[open..], "<Relationship", "Id")
}

/// Whether a new relationship in `rels_part` would really be written by
/// [`add_rel`] (which creates a missing part, so `must_exist` is `false`) or
/// [`add_workbook_rel`] (which doesn't): both splice it in before
/// `</Relationships>`, and on a part without one (truncated, a self-closed
/// or prefixed root) they still hand back an rId that names nothing. A
/// writer asks this before it writes anything that rId would be put in.
fn rels_takes(parts: &[(String, Vec<u8>)], rels_part: &str, must_exist: bool) -> bool {
    match parts.iter().find(|(n, _)| n == rels_part) {
        Some((_, bytes)) => String::from_utf8_lossy(bytes).contains("</Relationships>"),
        None => !must_exist,
    }
}

/// Add a relationship to any rels part (created when missing). Returns the
/// assigned rId ("" when the target is already related).
pub(crate) fn add_rel(
    parts: &mut Vec<(String, Vec<u8>)>,
    rels_part: &str,
    rel_type: &str,
    target: &str,
) -> String {
    if !parts.iter().any(|(n, _)| n == rels_part) {
        let empty = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>";
        parts.push((rels_part.to_string(), empty.as_bytes().to_vec()));
    }
    if let Some(p) = parts.iter_mut().find(|(n, _)| n == rels_part) {
        let xml = String::from_utf8_lossy(&p.1).into_owned();
        if xml.contains(&format!("Target=\"{target}\"")) {
            return String::new();
        }
        // Next free rIdN.
        let mut max = 0u32;
        let mut i = 0;
        while let Some(pos) = xml[i..].find("Id=\"rId") {
            let s = i + pos + "Id=\"rId".len();
            let digits: String = xml[s..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(n) = digits.parse::<u32>() {
                max = max.max(n);
            }
            i = s;
        }
        let rid = format!("rId{}", max + 1);
        let rel = format!("<Relationship Id=\"{rid}\" Type=\"{rel_type}\" Target=\"{target}\"/>");
        p.1 = xml
            .replacen("</Relationships>", &format!("{rel}</Relationships>"), 1)
            .into_bytes();
        return rid;
    }
    String::new()
}

pub(crate) fn add_workbook_rel(
    parts: &mut [(String, Vec<u8>)],
    rel_type: &str,
    target: &str,
) -> String {
    if let Some(p) = parts
        .iter_mut()
        .find(|(n, _)| n == "xl/_rels/workbook.xml.rels")
    {
        let xml = String::from_utf8_lossy(&p.1).into_owned();
        if xml.contains(&format!("Target=\"{target}\"")) {
            return String::new();
        }
        // Next free rIdN.
        let mut max = 0u32;
        let mut i = 0;
        while let Some(pos) = xml[i..].find("Id=\"rId") {
            let s = i + pos + "Id=\"rId".len();
            let digits: String = xml[s..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(n) = digits.parse::<u32>() {
                max = max.max(n);
            }
            i = s;
        }
        let rid = format!("rId{}", max + 1);
        let rel = format!("<Relationship Id=\"{rid}\" Type=\"{rel_type}\" Target=\"{target}\"/>");
        p.1 = xml
            .replacen("</Relationships>", &format!("{rel}</Relationships>"), 1)
            .into_bytes();
        return rid;
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Sheet management
// ---------------------------------------------------------------------------

impl SheetPackage {
    /// Append a blank sheet named `name`; returns its index. Wires up the
    /// part, content type, relationship, and the workbook.xml entry.
    pub fn add_sheet(&mut self, name: &str) -> usize {
        // Unused part name xl/worksheets/sheetN.xml.
        let mut n = 1;
        while self.part(&format!("xl/worksheets/sheet{n}.xml")).is_some() {
            n += 1;
        }
        let part_name = format!("xl/worksheets/sheet{n}.xml");
        let ns = self.ns();
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"{}\"><dimension ref=\"A1\"/><sheetData/></worksheet>",
            ns.sml
        );
        self.parts.push((part_name.clone(), body.into_bytes()));
        add_content_type_override(
            &mut self.parts,
            &format!("/{part_name}"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
        );
        let rid = add_workbook_rel(
            &mut self.parts,
            &ns.rel("worksheet"),
            &format!("worksheets/sheet{n}.xml"),
        );
        // workbook.xml <sheets> entry with the next free sheetId.
        if let Some(p) = self
            .parts
            .iter_mut()
            .find(|(pn, _)| pn == "xl/workbook.xml")
        {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            let mut max_id = 0u32;
            let mut i = 0;
            while let Some(pos) = xml[i..].find("sheetId=\"") {
                let s = i + pos + "sheetId=\"".len();
                let digits: String = xml[s..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(v) = digits.parse::<u32>() {
                    max_id = max_id.max(v);
                }
                i = s;
            }
            let entry = format!(
                "<sheet name=\"{}\" sheetId=\"{}\" r:id=\"{rid}\"/>",
                esc_attr(name),
                max_id + 1
            );
            p.1 = xml
                .replacen("</sheets>", &format!("{entry}</sheets>"), 1)
                .into_bytes();
        }
        self.workbook.sheets.push(Sheet {
            name: name.to_string(),
            ..Sheet::default()
        });
        self.sheet_parts.push(part_name);
        self.workbook.sheets.len() - 1
    }

    /// Add a `cellIs` conditional-formatting rule (Excel's "Highlight Cells"):
    /// the differential format `dxf` applies to `range` when the cell value
    /// satisfies `op` (greaterThan / lessThan / between / equal / …) against
    /// `formula1` (and `formula2` for `between`). Wires the OPC (a `<dxf>` in
    /// styles.xml + a `<conditionalFormatting>` in the worksheet) and the model,
    /// so it renders via cf::cell_dxf and round-trips.
    ///
    /// `false`, with nothing changed, when the rule can't be written: the
    /// sheet doesn't exist, or its worksheet part is malformed where the rule
    /// would go.
    pub fn add_conditional_format(
        &mut self,
        sheet: usize,
        range: (u32, u32, u32, u32),
        op: &str,
        formula1: &str,
        formula2: Option<&str>,
        dxf: crate::sheet::Dxf,
    ) -> bool {
        if sheet >= self.workbook.sheets.len()
            || !self.sheet_takes(sheet, "conditionalFormatting", false)
        {
            return false;
        }
        let dxf_id = self.workbook.styles.dxfs.len();
        self.workbook.styles.dxfs.push(dxf);
        // styles.xml: regenerate <dxfs> from the model (splice_styles preserves it).
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == "xl/styles.xml") {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            p.1 = set_dxfs(&xml, &self.workbook.styles.dxfs).into_bytes();
        }
        // Next-highest priority across the workbook (lower = higher precedence).
        let priority = self
            .workbook
            .sheets
            .iter()
            .flat_map(|s| s.cond_formats.iter())
            .flat_map(|cf| cf.rules.iter())
            .map(|r| r.priority)
            .max()
            .unwrap_or(0)
            + 1;
        // Worksheet: inject <conditionalFormatting> at its schema position,
        // after any existing ones.
        let (r1, c1, r2, c2) = range;
        let sqref = format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2));
        let mut fmls = format!("<formula>{}</formula>", esc_text(&file_formula(formula1)));
        if let Some(f2) = formula2 {
            fmls.push_str(&format!(
                "<formula>{}</formula>",
                esc_text(&file_formula(f2))
            ));
        }
        let cf_xml = format!(
            "<conditionalFormatting sqref=\"{sqref}\"><cfRule type=\"cellIs\" dxfId=\"{dxf_id}\" priority=\"{priority}\" operator=\"{op}\">{fmls}</cfRule></conditionalFormatting>"
        );
        let sheet_part = self.sheet_parts[sheet].clone();
        // (the new block's ordinal, how many there were before it)
        let mut placed = None;
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == sheet_part) {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            let old = cond_format_spans(&xml);
            let out = put_worksheet_child(&xml, "conditionalFormatting", &cf_xml, None, false);
            // It lands at its schema position: after the existing blocks in
            // a well-ordered part, but before any that follow a later-ranked
            // child in a misordered one. The blocks ahead of it keep their
            // starts, so the first start that differs is its ordinal.
            let new = cond_format_spans(&out);
            if new.len() == old.len() + 1 {
                let k = (0..old.len())
                    .find(|&i| new[i].0 != old[i].0)
                    .unwrap_or(old.len());
                placed = Some((k, old.len()));
            }
            p.1 = out.into_bytes();
        }
        // Model.
        let mut formulas = vec![formula1.to_string()];
        if let Some(f2) = formula2 {
            formulas.push(f2.to_string());
        }
        let rule = crate::sheet::CfRule {
            kind: crate::sheet::CfKind::CellIs {
                op: op.to_string(),
                formulas,
            },
            dxf_id: Some(dxf_id),
            priority,
        };
        let s = &mut self.workbook.sheets[sheet];
        let ix = placed.map(|(k, _)| k);
        if let Some((k, n)) = placed {
            // A claim on an ordinal the part didn't have (n or later) names
            // nothing (an undo restored the model but not the part): it goes.
            // The real blocks from the new one's place on moved up one.
            let renumber = |i: usize| match i {
                i if i >= n => None,
                i if i >= k => Some(i + 1),
                i => Some(i),
            };
            for cf in &mut s.cond_formats {
                cf.ix = cf.ix.and_then(renumber);
            }
            s.cf_removed = s.cf_removed.iter().filter_map(|&i| renumber(i)).collect();
        }
        s.cond_formats.push(crate::sheet::CondFormat {
            ranges: vec![range],
            rules: vec![rule],
            ix,
        });
        true
    }

    /// Whether the worksheet part of `sheet` can hold a `<dataValidations>`
    /// block, so a rule added to the model will be written by a save. A
    /// host refuses to add one when it can't, as it does for other edits it
    /// cannot write.
    pub fn takes_validations(&self, sheet: usize) -> bool {
        sheet < self.workbook.sheets.len() && self.sheet_takes(sheet, "dataValidations", true)
    }

    /// Add a data-validation rule to `sheet` over `range`. For a list, pass
    /// kind="list" and formula1 as an inline `"a,b,c"` list or a range ref;
    /// numeric/date kinds use an operator (between/greaterThan/…) + operand(s).
    /// Appends a `<dataValidation>` to the worksheet (preserving existing ones)
    /// and the model, so it round-trips and drives the dropdown / cell check.
    /// `false`, with nothing changed, when it can't be written (see
    /// [`SheetPackage::add_conditional_format`]).
    pub fn add_data_validation(
        &mut self,
        sheet: usize,
        range: (u32, u32, u32, u32),
        kind: &str,
        operator: &str,
        formula1: &str,
        formula2: Option<&str>,
    ) -> bool {
        self.add_data_validations(
            sheet,
            &[NewValidation {
                range,
                kind,
                operator,
                formula1,
                formula2,
                settings: None,
            }],
        )
    }

    /// [`SheetPackage::add_data_validation`] for several rules at once, in
    /// order: the worksheet part is rewritten once, not once per rule, so a
    /// paste of many rules stays linear (#707 r6).
    pub fn add_data_validations(&mut self, sheet: usize, rules: &[NewValidation]) -> bool {
        if sheet >= self.workbook.sheets.len() || !self.sheet_takes(sheet, "dataValidations", true)
        {
            return false;
        }
        if rules.is_empty() {
            return true;
        }
        // The rule each entry stands for, as the model holds it.
        let model: Vec<crate::sheet::DataValidation> = rules
            .iter()
            .map(|r| {
                let base = r
                    .settings
                    .cloned()
                    .unwrap_or_else(|| crate::sheet::DataValidation {
                        allow_blank: true,
                        show_input: true,
                        show_error: true,
                        ..Default::default()
                    });
                crate::sheet::DataValidation {
                    ranges: vec![r.range],
                    kind: r.kind.to_string(),
                    operator: r.operator.to_string(),
                    formula1: r.formula1.to_string(),
                    formula2: r.formula2.unwrap_or("").to_string(),
                    ix: None,
                    orig: None,
                    ..base
                }
            })
            .collect();
        let items: Vec<String> = model.iter().map(dv_element).collect();
        let n = items.len();
        let sheet_part = self.sheet_parts[sheet].clone();
        let mut first_ix = None;
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == sheet_part) {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            let before = validation_spans(&xml).map_or(0, |(_, items)| items.len());
            // Into the existing block (in any prefix), bumping its count, or a
            // new block at its schema position.
            let xml = append_all_to_worksheet_child(&xml, "dataValidations", &items)
                .unwrap_or_else(|| {
                    let block = format!(
                        "<dataValidations count=\"{n}\">{}</dataValidations>",
                        items.concat()
                    );
                    put_worksheet_child(&xml, "dataValidations", &block, None, false)
                });
            // Appended before the block's end tag, so after its existing
            // rules: the next ordinals, as long as they are really there.
            let after = validation_spans(&xml).map_or(0, |(_, items)| items.len());
            first_ix = (after == before + n).then_some(before);
            p.1 = xml.into_bytes();
        }
        let s = &mut self.workbook.sheets[sheet];
        if let Some(first) = first_ix {
            // Stale claims go, as in `add_conditional_format`.
            for dv in &mut s.validations {
                dv.ix = dv.ix.filter(|&i| i < first);
            }
            s.dv_removed.retain(|&i| i < first);
        }
        for (k, mut dv) in model.into_iter().enumerate() {
            dv.ix = first_ix.map(|f| f + k);
            // The element written above is the rule's original, when it is there.
            if dv.ix.is_some() {
                dv.orig = Some(Box::new(dv.clone()));
            }
            s.validations.push(dv);
        }
        true
    }

    /// Create an Excel Table ("Format as Table") over `range`. Column names come
    /// from the header row when `has_header` (else "Column1"…). Wires the OPC (a
    /// `xl/tables/table*.xml` part, its content type, a worksheet `/table` rel +
    /// `<tableParts>`) and the model, so it round-trips and Excel styles it with
    /// the given `style` (e.g. "TableStyleMedium2"). Returns the table index,
    /// or why it can't: as Excel does, a table can't overlap another table, a
    /// PivotTable or part of a multi-cell array formula
    /// ([`crate::edit::table_range_conflict`]).
    pub fn add_table(
        &mut self,
        sheet: usize,
        range: (u32, u32, u32, u32),
        has_header: bool,
        style: &str,
    ) -> Result<usize, String> {
        // Asked before any part, rel or content type is written, so a refusal
        // leaves the package exactly as it was.
        if sheet >= self.workbook.sheets.len() {
            return Err("There is no such sheet".into());
        }
        if let Some(why) = crate::edit::table_range_conflict(&self.workbook, sheet, range, None) {
            return Err(why);
        }
        if !self.sheet_takes(sheet, "tableParts", true) {
            return Err("This sheet's file can't take a table".into());
        }
        let (r1, c1, r2, c2) = range;
        let mut tn = 1;
        while self.part(&format!("xl/tables/table{tn}.xml")).is_some() {
            tn += 1;
        }
        // Column names: from the header row (deduped), else generated.
        let mut names: Vec<String> = Vec::new();
        for c in c1..=c2 {
            let header = has_header
                .then(|| self.workbook.sheets[sheet].cell(r1, c).map(|cl| &cl.value))
                .flatten();
            names.push(crate::edit::table_column_name(header, c - c1 + 1, &names));
        }
        // Unique table display name: names compare case-insensitively, and
        // a table can't take a defined name's.
        let wb = &self.workbook;
        let mut k = wb.tables.len() + 1;
        let name = loop {
            let cand = format!("Table{k}");
            let used = wb.tables.iter().map(|t| &t.name);
            let used = used.chain(wb.defined_names.iter().map(|d| &d.name));
            if !used.into_iter().any(|n| n.eq_ignore_ascii_case(&cand)) {
                break cand;
            }
            k += 1;
        };
        let sqref = format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2));
        let cols_xml: String = names
            .iter()
            .enumerate()
            .map(|(i, n)| format!("<tableColumn id=\"{}\" name=\"{}\"/>", i + 1, esc_attr(n)))
            .collect();
        let header_attr = if has_header {
            ""
        } else {
            " headerRowCount=\"0\""
        };
        let ns = self.ns();
        let table_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<table xmlns=\"{}\" id=\"{tn}\" name=\"{name}\" displayName=\"{name}\" ref=\"{sqref}\"{header_attr} totalsRowShown=\"0\"><autoFilter ref=\"{sqref}\"/><tableColumns count=\"{}\">{cols_xml}</tableColumns><tableStyleInfo name=\"{style}\" showFirstColumn=\"0\" showLastColumn=\"0\" showRowStripes=\"1\" showColumnStripes=\"0\"/></table>",
            ns.sml,
            names.len()
        );
        let part = format!("xl/tables/table{tn}.xml");
        self.parts.push((part.clone(), table_xml.into_bytes()));
        add_content_type_override(
            &mut self.parts,
            &format!("/{part}"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml",
        );

        // Worksheet rel → table + a <tableParts> entry (Excel needs both).
        let sheet_part = self.sheet_parts[sheet].clone();
        let (ws_dir, ws_file) = sheet_part
            .rsplit_once('/')
            .unwrap_or(("", sheet_part.as_str()));
        let rels_part = format!("{ws_dir}/_rels/{ws_file}.rels");
        let rid = add_rel(
            &mut self.parts,
            &rels_part,
            &ns.rel("table"),
            &format!("../tables/table{tn}.xml"),
        );
        if !rid.is_empty() {
            if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == sheet_part) {
                let xml = String::from_utf8_lossy(&p.1).into_owned();
                let entry = format!("<tablePart r:id=\"{rid}\"/>");
                let rels = Some(ns.rels);
                let xml = append_to_worksheet_child(&xml, "tableParts", &entry, rels)
                    .unwrap_or_else(|| {
                        let block = format!("<tableParts count=\"1\">{entry}</tableParts>");
                        put_worksheet_child(&xml, "tableParts", &block, rels, false)
                    });
                p.1 = xml.into_bytes();
            }
        }
        self.workbook.tables.push(crate::sheet::Table {
            name,
            sheet,
            range,
            header_rows: u32::from(has_header),
            totals_rows: 0,
            column_ids: (1..=names.len() as u32).collect(),
            calculated_formulas: Vec::new(),
            columns: names,
            part,
        });
        Ok(self.workbook.tables.len() - 1)
    }

    /// Remove all conditional-formatting rules from `sheet` (model + the
    /// worksheet's `<conditionalFormatting>` elements). Orphaned `<dxf>`s are left
    /// in styles.xml — harmless and referenced by nothing.
    ///
    /// On a malformed worksheet part it clears only when the walk got past
    /// every place a block may stand (it stopped at a child ranking after
    /// `conditionalFormatting`). Otherwise it returns `false` and changes
    /// neither the part nor the model, rather than claim a clear the file
    /// doesn't carry.
    pub fn clear_conditional_formats(&mut self, sheet: usize) -> bool {
        if sheet >= self.workbook.sheets.len() {
            return false;
        }
        let sheet_part = self.sheet_parts[sheet].clone();
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == sheet_part) {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            // Every top-level block, in any prefix; never an x14 one in extLst.
            // A known position for a new block means the walk got past every
            // place an existing one may stand.
            if worksheet_insert_pos(&xml, "conditionalFormatting").is_none() {
                return false;
            }
            let walk = worksheet_children(&xml);
            let mut out = xml.clone();
            for c in walk
                .children
                .iter()
                .rev()
                .filter(|c| c.local == "conditionalFormatting")
            {
                out.replace_range(c.start..c.end, "");
            }
            p.1 = out.into_bytes();
        }
        let s = &mut self.workbook.sheets[sheet];
        s.cond_formats.clear();
        // The elements those named are gone from the part.
        s.cf_removed.clear();
        true
    }

    /// Would [`add_chart`](Self::add_chart) on `sheet` get past its checks:
    /// the sheet exists, its worksheet part has a known place for the
    /// `<drawing>`, and a drawing part it already has can take an anchor?
    /// And can the rels parts take the relationships the chart needs: the
    /// drawing's to the chart, and the worksheet's to a new drawing? A
    /// caller that keeps a chart to write later (docxy writes UI charts at
    /// save) asks this when the chart is made, so a refusal is reported then.
    pub fn can_add_chart(&self, sheet: usize) -> bool {
        // add_chart names the sheet's part: a sheet the model has but the
        // package lists no part for (an undo that brought back a removed
        // sheet restores the model only) can't take one.
        if sheet >= self.workbook.sheets.len()
            || sheet >= self.sheet_parts.len()
            || !self.sheet_takes(sheet, "drawing", true)
        {
            return false;
        }
        let host = self.chart_host(sheet);
        // A host part with neither a `</wsDr>` nor a self-closed root to open
        // is truncated or isn't a drawing.
        let host_takes = match host.as_deref().and_then(|p| self.part(p)) {
            Some(xml) => {
                let xml = String::from_utf8_lossy(xml);
                let px = wsdr_prefix(&xml);
                xml.rfind(&format!("</{px}wsDr>")).is_some()
                    || open_self_closed_root(&xml, &format!("{px}wsDr")).is_some()
            }
            None => true,
        };
        // The chart's rel goes into the drawing's rels part: the host's, or
        // the one a new drawing part's name implies, which an orphan left
        // behind may already hold. The worksheet's rel to the drawing is
        // reused when the host already has one, and otherwise written.
        let sheet_part = &self.sheet_parts[sheet];
        let ws_rels = rels_part_name(sheet_part);
        let (ws_dir, _) = sheet_part.rsplit_once('/').unwrap_or(("", sheet_part));
        let rels_take = match &host {
            Some(h) => {
                rels_takes(&self.parts, &rels_part_name(h), false)
                    && (rel_id_for(&self.parts, &ws_rels, &relative_target(ws_dir, h)).is_some()
                        || rels_takes(&self.parts, &ws_rels, false))
            }
            None => {
                rels_takes(
                    &self.parts,
                    &rels_part_name(&self.new_drawing_part()),
                    false,
                ) && rels_takes(&self.parts, &ws_rels, false)
            }
        };
        host_takes && rels_take
    }

    /// The part name a new drawing takes: the first free
    /// `xl/drawings/drawingN.xml`.
    fn new_drawing_part(&self) -> String {
        let mut dn = 1;
        while self.part(&format!("xl/drawings/drawing{dn}.xml")).is_some() {
            dn += 1;
        }
        format!("xl/drawings/drawing{dn}.xml")
    }

    /// The drawing part `sheet` already has, which a new chart joins.
    fn chart_host(&self, sheet: usize) -> Option<String> {
        self.workbook.sheets[sheet]
            .drawing_part
            .clone()
            .filter(|p| self.part(p).is_some())
    }

    /// Write a clustered column chart (cached literal data, self-contained) onto
    /// `sheet`, anchored over the cell rect `from`..`to`, wiring the full OPC:
    /// the chart part, a drawing part with a twoCellAnchor graphicFrame, both
    /// rels, the content-type overrides, and the worksheet's `<drawing>` element.
    /// Also registers it in the model so it round-trips on reload.
    ///
    /// `false`, with nothing changed, when the chart can't be written: the
    /// worksheet part (malformed where `<drawing>` would go) and the host
    /// drawing part (no root to splice the anchor into) are asked before any
    /// part, rel or content type is written ([`can_add_chart`](Self::can_add_chart)).
    pub fn add_chart(
        &mut self,
        sheet: usize,
        from: (u32, u32),
        to: (u32, u32),
        data: &crate::sheet::ChartData,
    ) -> bool {
        if !self.can_add_chart(sheet) {
            return false;
        }
        let ns = self.ns();
        let mut cn = 1;
        while self.part(&format!("xl/charts/chart{cn}.xml")).is_some() {
            cn += 1;
        }
        let chart_part = format!("xl/charts/chart{cn}.xml");

        // 1) where the anchor goes. A worksheet may carry only ONE `<drawing>`,
        // so a fresh part for a sheet that already has one would be orphaned:
        // we'd still read it back, Excel would show only the part the worksheet
        // names. A second chart — or the first on a sheet that already holds a
        // picture — therefore joins the part that is already there.
        let host = self.chart_host(sheet);
        let drawing_part = host.clone().unwrap_or_else(|| self.new_drawing_part());
        let (d_dir, d_file) = drawing_part
            .rsplit_once('/')
            .unwrap_or(("", drawing_part.as_str()));
        let (d_dir, d_file) = (d_dir.to_string(), d_file.to_string());

        // 2) drawing rels → chart (its rId names the chart from the anchor).
        // Minted BEFORE the chart part is written, so a failure here leaves no
        // orphan behind.
        let d_rels = format!("{d_dir}/_rels/{d_file}.rels");
        let c_target = relative_target(&d_dir, &chart_part);
        let c_rid = match add_rel(&mut self.parts, &d_rels, &ns.rel("chart"), &c_target) {
            // "" means the rel was already there — a dangling one naming a chart
            // part the zip doesn't hold, so `cn` picked its name. Reuse its id;
            // an anchor written with `r:id=""` makes Excel call the whole
            // workbook unreadable.
            id if id.is_empty() => rel_id_for(&self.parts, &d_rels, &c_target).unwrap_or_default(),
            id => id,
        };
        if c_rid.is_empty() {
            return false; // no id to point the graphic frame at; leave the file alone
        }

        // 3) chart part + content type.
        self.parts.push((
            chart_part.clone(),
            chart_space_xml_in(data, ns).into_bytes(),
        ));
        add_content_type_override(
            &mut self.parts,
            &format!("/{chart_part}"),
            "application/vnd.openxmlformats-officedocument.drawingml.chart+xml",
        );

        // 4) the anchor itself, in whatever prefix the host part uses (`xdr:`
        // for anything Excel wrote, but a part with a default namespace has
        // none). `a` and `r` are declared on the anchor, so it stands alone
        // whatever the root does or doesn't declare.
        let host_xml = host
            .as_deref()
            .and_then(|p| self.part(p))
            .map(|b| String::from_utf8_lossy(b).into_owned());
        let px = host_xml
            .as_deref()
            .map(wsdr_prefix)
            .unwrap_or("xdr:")
            .to_string();
        // `cNvPr/@id` is unique WITHIN a drawing part, and now that we splice
        // into an existing one the chart's part number says nothing about the
        // ids already in it (Excel numbers its first picture `2`). A duplicate
        // is a known repair trigger, so take one past the highest there.
        let shape_id = host_xml
            .as_deref()
            .map(|xml| {
                let mut max = 1u32;
                let mut at = 0usize;
                while let Some(i) = xml[at..].find("cNvPr ").map(|i| at + i + 6) {
                    at = i;
                    if let Some(v) = xml[i..].find("id=\"").map(|j| i + j + 4) {
                        let n: u32 = xml[v..]
                            .split('"')
                            .next()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        max = max.max(n);
                    }
                }
                // A part is free to spell an id right up at the ceiling; `+ 1`
                // would panic in debug and wrap to 0 in release, minting exactly
                // the duplicate id this scan exists to avoid.
                max.saturating_add(1)
            })
            .unwrap_or(cn + 1);
        let (fr, fc) = from;
        let (tr, tc) = to;
        let anchor = format!(
            "<{px}twoCellAnchor xmlns:a=\"{dml}\" xmlns:r=\"{rels}\">\
<{px}from><{px}col>{fc}</{px}col><{px}colOff>0</{px}colOff><{px}row>{fr}</{px}row><{px}rowOff>0</{px}rowOff></{px}from>\
<{px}to><{px}col>{tc}</{px}col><{px}colOff>0</{px}colOff><{px}row>{tr}</{px}row><{px}rowOff>0</{px}rowOff></{px}to>\
<{px}graphicFrame macro=\"\"><{px}nvGraphicFramePr><{px}cNvPr id=\"{id}\" name=\"Chart {cn}\"/><{px}cNvGraphicFramePr/></{px}nvGraphicFramePr>\
<{px}xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></{px}xfrm>\
<a:graphic><a:graphicData uri=\"{chart}\"><c:chart xmlns:c=\"{chart}\" r:id=\"{c_rid}\"/></a:graphicData></a:graphic></{px}graphicFrame>\
<{px}clientData/></{px}twoCellAnchor>",
            id = shape_id,
            dml = ns.dml,
            rels = ns.rels,
            chart = ns.chart,
        );
        // Where this anchor will land: it is spliced at the end, so it takes the
        // next free index in whichever part hosts it. Knowing it is what lets a
        // later move or delete address this drawing like any other.
        let anchor_ix = host_xml
            .as_deref()
            .map(crate::drawing::count_anchors)
            .unwrap_or(0);
        match host_xml {
            // Splice before the root's close tag, so the anchors already there
            // keep their indices (the save-side rewrite is keyed by them).
            Some(xml) => {
                let close = format!("</{px}wsDr>");
                let spliced = match xml.rfind(&close) {
                    Some(at) => Some(format!("{}{anchor}{}", &xml[..at], &xml[at..])),
                    // No close tag. A part whose last shape was deleted can be
                    // written as a self-closed empty root — `<xdr:wsDr …/>` —
                    // which is legal; open it up and put the anchor inside.
                    // Appending after it instead would give the part TWO
                    // top-level elements, and Excel calls that unreadable.
                    None => open_self_closed_root(&xml, &format!("{px}wsDr"))
                        .map(|opened| format!("{opened}{anchor}</{px}wsDr>")),
                };
                match spliced {
                    Some(spliced) => {
                        if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == drawing_part) {
                            p.1 = spliced.into_bytes();
                        }
                    }
                    // Neither form of root found: the part is truncated or isn't
                    // a drawing at all. A bare anchor appended to it would be a
                    // fragment with an undeclared prefix, so leave it be. The
                    // chart part written above is then simply unreferenced,
                    // which is valid OPC and which Excel ignores.
                    None => return false,
                }
            }
            None => {
                let drawing_xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<xdr:wsDr xmlns:xdr=\"{}\" xmlns:a=\"{}\" xmlns:r=\"{}\">\
{anchor}</xdr:wsDr>",
                    ns.xdr, ns.dml, ns.rels
                );
                self.parts
                    .push((drawing_part.clone(), drawing_xml.into_bytes()));
                add_content_type_override(
                    &mut self.parts,
                    &format!("/{drawing_part}"),
                    "application/vnd.openxmlformats-officedocument.drawing+xml",
                );
                // The next chart on this sheet joins the part we just wrote
                // rather than minting another one the worksheet can't name.
                self.workbook.sheets[sheet].drawing_part = Some(drawing_part.clone());
            }
        }

        // 5) the worksheet points at that part — normally already true for a
        // host part, but a model that named one the worksheet never referenced
        // would otherwise save a drawing nothing can reach.
        let sheet_part = self.sheet_parts[sheet].clone();
        let (ws_dir, ws_file) = sheet_part
            .rsplit_once('/')
            .unwrap_or(("", sheet_part.as_str()));
        let rels_part = format!("{ws_dir}/_rels/{ws_file}.rels");
        let target = relative_target(ws_dir, &drawing_part);
        let rid = match add_rel(&mut self.parts, &rels_part, &ns.rel("drawing"), &target) {
            // "" means the rel was already there; reuse its id.
            id if id.is_empty() => rel_id_for(&self.parts, &rels_part, &target).unwrap_or_default(),
            id => id,
        };
        if !rid.is_empty() {
            if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == sheet_part) {
                let xml = String::from_utf8_lossy(&p.1).into_owned();
                // CT_Worksheet is a SEQUENCE: `drawing` comes before
                // `legacyDrawing`, `picture`, `oleObjects`, `controls`,
                // `tableParts` and `extLst`. Appending at `</worksheet>`
                // puts it after any of those, and Excel treats an
                // out-of-order child as unreadable content — it "repairs"
                // the file by dropping the drawing or the table.
                //
                // A worksheet that already names a drawing the LOADER rejected
                // — a rel that resolves to nothing, or a part missing from the
                // zip — had none in the model, so we just minted a fresh one. A
                // worksheet may carry only one `<drawing>`, so leaving the
                // stale element in place orphans the part we wrote: the chart
                // the user inserted would be silently absent from the saved
                // file. Point the element at the new part instead.
                let has_drawing = worksheet_child_span(&xml, "drawing").is_some();
                p.1 = if !has_drawing || host.is_none() {
                    let block = format!("<drawing r:id=\"{rid}\"/>");
                    put_worksheet_child(&xml, "drawing", &block, Some(ns.rels), true)
                } else {
                    xml
                }
                .into_bytes();
            }
        }

        // The model records where the chart actually lives: the anchor's index in
        // the host part, so moving or deleting it addresses the right element,
        // and the part we just wrote, so an edit to it can be regenerated. A
        // caller cloning another chart's data would otherwise leave `part`
        // pointing at the ORIGINAL, and editing the copy would overwrite it.
        let mut data = data.clone();
        data.part = Some(chart_part.clone());
        self.workbook.sheets[sheet]
            .drawings
            .push(crate::sheet::Drawing {
                anchor_ix,
                from,
                to,
                kind: crate::sheet::DrawingKind::Chart(data),
            });
        true
    }

    /// Create a pivot table from scratch: writes a pivotCacheDefinition and
    /// pivotTableDefinition part with full OPC wiring (content types,
    /// workbook `<pivotCaches>`, workbook rels, destination-sheet rels) and
    /// registers the pivot in the model with `edited = true`, so save
    /// rewrites the field layout from whatever the editor sets up. Returns
    /// the index into `workbook.pivots`.
    pub fn add_pivot(
        &mut self,
        source: crate::pivot::PivotSource,
        fields: Vec<String>,
        default_measure: crate::pivot::DataField,
        dest_sheet: usize,
        location: (u32, u32),
    ) -> Option<usize> {
        if dest_sheet >= self.workbook.sheets.len() || fields.is_empty() {
            return None;
        }
        // The cache's rel (workbook rels) and the table's (the destination
        // sheet's rels) must be writable before any part is: a cache whose
        // `<pivotCache r:id>` names nothing, or a table part no sheet reaches,
        // is worse than no pivot.
        let sheet_part = self.sheet_parts.get(dest_sheet)?.clone();
        if !rels_takes(&self.parts, "xl/_rels/workbook.xml.rels", true)
            || !rels_takes(&self.parts, &rels_part_name(&sheet_part), false)
        {
            return None;
        }
        // Unused part names + the next free cacheId.
        let mut n = 1;
        while self
            .part(&format!("xl/pivotTables/pivotTable{n}.xml"))
            .is_some()
        {
            n += 1;
        }
        let mut m = 1;
        while self
            .part(&format!("xl/pivotCache/pivotCacheDefinition{m}.xml"))
            .is_some()
        {
            m += 1;
        }
        let table_part = format!("xl/pivotTables/pivotTable{n}.xml");
        let cache_part = format!("xl/pivotCache/pivotCacheDefinition{m}.xml");
        let mut cache_id = 1u32;
        if let Some(bytes) = self.part("xl/workbook.xml") {
            let xml = String::from_utf8_lossy(bytes);
            let mut i = 0;
            while let Some(pos) = xml[i..].find("cacheId=\"") {
                let s = i + pos + "cacheId=\"".len();
                let digits: String = xml[s..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(v) = digits.parse::<u32>() {
                    cache_id = cache_id.max(v + 1);
                }
                i = s;
            }
        }

        // The cache: source + field names. refreshOnLoad makes Excel build
        // its own records; we never write a records part.
        let source_xml = match &source {
            crate::pivot::PivotSource::Range { sheet, rect } => {
                let (r1, c1, r2, c2) = *rect;
                format!(
                    "<worksheetSource ref=\"{}:{}\" sheet=\"{}\"/>",
                    cell_name(r1, c1),
                    cell_name(r2, c2),
                    esc_attr(sheet)
                )
            }
            crate::pivot::PivotSource::Table(name) => {
                format!("<worksheetSource name=\"{}\"/>", esc_attr(name))
            }
        };
        let ns = self.ns();
        let mut cache_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<pivotCacheDefinition xmlns=\"{}\" refreshOnLoad=\"1\" recordCount=\"0\"><cacheSource type=\"worksheet\">{source_xml}</cacheSource><cacheFields count=\"{}\">",
            ns.sml,
            fields.len()
        );
        for f in &fields {
            cache_xml.push_str(&format!(
                "<cacheField name=\"{}\" numFmtId=\"0\"><sharedItems/></cacheField>",
                esc_attr(f)
            ));
        }
        cache_xml.push_str("</cacheFields></pivotCacheDefinition>");
        self.parts
            .push((cache_part.clone(), cache_xml.into_bytes()));
        add_content_type_override(
            &mut self.parts,
            &format!("/{cache_part}"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheDefinition+xml",
        );
        let cache_target = format!("pivotCache/pivotCacheDefinition{m}.xml");
        let cache_rid = match add_workbook_rel(
            &mut self.parts,
            &ns.rel("pivotCacheDefinition"),
            &cache_target,
        ) {
            // "" means the rel was already there (a dangling one naming the
            // part name `m` picked): reuse its id.
            id if id.is_empty() => {
                rel_id_for(&self.parts, "xl/_rels/workbook.xml.rels", &cache_target)
                    .unwrap_or_default()
            }
            id => id,
        };

        // workbook.xml: register the cache.
        if let Some(p) = self
            .parts
            .iter_mut()
            .find(|(pn, _)| pn == "xl/workbook.xml")
        {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            p.1 = register_pivot_cache(&xml, cache_id, &cache_rid, ns.rels).into_bytes();
        }

        // The pivot definition. Save rewrites the field layout (the pivot is
        // registered as edited), so this base only needs valid structure.
        let (lr, lc) = location;
        let loc_ref = format!("{}:{}", cell_name(lr, lc), cell_name(lr + 1, lc + 1));
        let mut table_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<pivotTableDefinition xmlns=\"{}\" name=\"PivotTable{n}\" cacheId=\"{cache_id}\" dataCaption=\"Values\" useAutoFormatting=\"1\" indent=\"0\" outline=\"1\" outlineData=\"1\"><location ref=\"{loc_ref}\" firstHeaderRow=\"1\" firstDataRow=\"1\" firstDataCol=\"1\"/><pivotFields count=\"{}\">",
            ns.sml,
            fields.len()
        );
        for (i, _) in fields.iter().enumerate() {
            if i == default_measure.field {
                table_xml.push_str("<pivotField dataField=\"1\" showAll=\"0\"/>");
            } else {
                table_xml.push_str("<pivotField showAll=\"0\"/>");
            }
        }
        table_xml.push_str(&format!(
            "</pivotFields><dataFields count=\"1\"><dataField name=\"{}\" fld=\"{}\" baseField=\"0\" baseItem=\"0\"/></dataFields><pivotTableStyleInfo name=\"PivotStyleLight16\" showRowHeaders=\"1\" showColHeaders=\"1\" showRowStripes=\"0\" showColStripes=\"0\" showLastColumn=\"1\"/></pivotTableDefinition>",
            esc_attr(&default_measure.name),
            default_measure.field
        ));
        self.parts
            .push((table_part.clone(), table_xml.into_bytes()));
        add_content_type_override(
            &mut self.parts,
            &format!("/{table_part}"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.pivotTable+xml",
        );

        // Destination sheet's rels → the pivot part.
        let sheet_part = &self.sheet_parts[dest_sheet];
        let ws_dir = sheet_part.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let ws_file = sheet_part
            .rsplit_once('/')
            .map(|(_, f)| f)
            .unwrap_or(sheet_part);
        let rels_part = format!("{ws_dir}/_rels/{ws_file}.rels");
        add_rel(
            &mut self.parts,
            &rels_part,
            &ns.rel("pivotTable"),
            &format!("../pivotTables/pivotTable{n}.xml"),
        );

        self.workbook.pivots.push(crate::pivot::Pivot {
            name: format!("PivotTable{n}"),
            sheet: dest_sheet,
            location: (lr, lc, lr + 1, lc + 1),
            source,
            fields,
            row_fields: Vec::new(),
            col_fields: Vec::new(),
            data_fields: vec![default_measure],
            field_items: Vec::new(),
            hidden: Vec::new(),
            page: Vec::new(),
            items_order: Vec::new(),
            calc_formulas: Vec::new(),
            grand_rows: true,
            grand_cols: true,
            subtotals: false,
            data_on_rows: false,
            unsupported: false,
            edited: true,
            part: table_part,
            cache_part,
        });
        Some(self.workbook.pivots.len() - 1)
    }

    /// Build a REAL, persistent pivot with the full row/col/value layout
    /// already applied — the constructor-style counterpart to `add_pivot` +
    /// the TUI's interactive field editor (Ctrl-P then r/c/v per field).
    /// Lands the output on a new sheet named `sheet_name` and refreshes it,
    /// so the caller gets back computed values, not just an empty shell.
    /// `spec`'s row/col/measure indices must already be resolved against
    /// `frame` (e.g. via `pivot_spec_from_names`). Returns the new pivot's
    /// index into `workbook.pivots` (`.sheet` on it is the destination
    /// sheet); `None` when the source has no headers/rows or no measures.
    pub fn create_pivot(
        &mut self,
        source: crate::pivot::PivotSource,
        frame: &crate::frame::Frame,
        spec: &crate::frame::PivotSpec,
        sheet_name: &str,
    ) -> Option<usize> {
        if frame.names.is_empty() || frame.rows() == 0 || spec.measures.is_empty() {
            return None;
        }
        let data_fields: Vec<crate::pivot::DataField> = spec
            .measures
            .iter()
            .map(|m| crate::pivot::DataField {
                name: m.name.clone(),
                field: m.col,
                agg: m.agg,
            })
            .collect();
        let dest = self.add_sheet(sheet_name);
        // Infallible: `add_pivot` only returns `None` for an out-of-range
        // `dest_sheet` or empty `fields`. `dest` was just pushed by
        // `add_sheet` above (always in range), and `fields` is
        // `frame.names`, already checked non-empty by the guard at the top
        // of this function — so a `?` here would be silently unreachable
        // AND, on the impossible path, leak the freshly-added `dest` sheet
        // (never removed, no pivot registered). `expect` makes the
        // impossibility explicit instead.
        let idx = self
            .add_pivot(
                source,
                frame.names.clone(),
                data_fields[0].clone(),
                dest,
                (2, 0),
            )
            .expect("dest just created (in range) and fields is frame.names (non-empty)");
        let p = &mut self.workbook.pivots[idx];
        p.row_fields = spec.rows.clone();
        p.col_fields = spec.cols.clone();
        p.data_fields = data_fields;
        p.edited = true;
        crate::pivot::refresh_pivots(&mut self.workbook);
        Some(idx)
    }

    /// Remove pivot `idx` from `workbook.pivots`: drops its table/cache
    /// parts, their content-type overrides, the workbook-rels relationship +
    /// `<pivotCache>` registration for the cache, and the destination
    /// sheet's relationship to the table part. The exact inverse of
    /// `add_pivot`/`create_pivot` — nothing else in the codebase unregisters
    /// a pivot, so this is the "remove both or neither" half of the
    /// pivot-creation inverse (pair with `remove_sheet` when the pivot's
    /// sheet was created solely to hold it). Returns false when `idx` is out
    /// of range.
    pub fn remove_pivot(&mut self, idx: usize) -> bool {
        if idx >= self.workbook.pivots.len() {
            return false;
        }
        let piv = self.workbook.pivots.remove(idx);
        self.parts
            .retain(|(n, _)| *n != piv.part && *n != piv.cache_part);
        if let Some(p) = self
            .parts
            .iter_mut()
            .find(|(n, _)| n == "[Content_Types].xml")
        {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            let xml = remove_element_containing(&xml, "<Override", &format!("/{}", piv.part));
            let xml = remove_element_containing(&xml, "<Override", &format!("/{}", piv.cache_part));
            p.1 = xml.into_bytes();
        }
        // workbook.xml.rels: drop the cache relationship, remembering its rId.
        let cache_target = piv.cache_part.trim_start_matches("xl/").to_string();
        let mut cache_rid = String::new();
        if let Some(p) = self
            .parts
            .iter_mut()
            .find(|(n, _)| n == "xl/_rels/workbook.xml.rels")
        {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            if let Some(rel_pos) = xml.find(&format!("Target=\"{cache_target}\"")) {
                if let Some(id_pos) = xml[..rel_pos].rfind("Id=\"") {
                    let s = id_pos + 4;
                    if let Some(e) = xml[s..].find('\"') {
                        cache_rid = xml[s..s + e].to_string();
                    }
                }
            }
            p.1 = remove_element_containing(
                &xml,
                "<Relationship",
                &format!("Target=\"{cache_target}\""),
            )
            .into_bytes();
        }
        // workbook.xml: drop the <pivotCache> entry that referenced it.
        if !cache_rid.is_empty() {
            if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == "xl/workbook.xml") {
                let xml = String::from_utf8_lossy(&p.1).into_owned();
                p.1 = unregister_pivot_cache(&xml, &cache_rid).into_bytes();
            }
        }
        // Destination sheet's rels: drop its relationship to the table part.
        if let Some(sheet_part) = self.sheet_parts.get(piv.sheet) {
            let ws_dir = sheet_part.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            let ws_file = sheet_part
                .rsplit_once('/')
                .map(|(_, f)| f)
                .unwrap_or(sheet_part);
            let rels_part = format!("{ws_dir}/_rels/{ws_file}.rels");
            let table_file = piv
                .part
                .rsplit_once('/')
                .map(|(_, f)| f)
                .unwrap_or(&piv.part);
            if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == &rels_part) {
                let xml = String::from_utf8_lossy(&p.1).into_owned();
                p.1 = remove_element_containing(&xml, "<Relationship", table_file).into_bytes();
            }
        }
        true
    }

    /// Remove the sheet at `idx`. Returns false (and does nothing) when it
    /// is the last sheet — a workbook must keep at least one.
    ///
    /// Any pivot whose output lives on `idx` goes with it — a sheet-only
    /// removal would otherwise leave its `workbook.pivots` entry dangling
    /// (Wave-3's "remove both or neither" rule for `pivot.create`'s
    /// inverse). Pivots on later sheets keep pointing at the right sheet as
    /// indices shift down, same as `defined_names` scopes below.
    /// Rename sheet `idx`, updating the model, any formulas that reference the
    /// old sheet name (via [`crate::edit::rename_sheet`]), and the `<sheet
    /// name="…">` entries in `workbook.xml`. Rejects a name that collides
    /// (case-insensitively) with another sheet, or an out-of-range / empty
    /// name. Returns whether the rename happened.
    pub fn rename_sheet(&mut self, idx: usize, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || idx >= self.workbook.sheets.len() {
            return false;
        }
        if self
            .workbook
            .sheets
            .iter()
            .enumerate()
            .any(|(i, s)| i != idx && s.name.eq_ignore_ascii_case(name))
        {
            return false;
        }
        // Model + cross-sheet formula references.
        crate::edit::rename_sheet(&mut self.workbook, idx, name);
        // Re-sync workbook.xml from the model (same helper save_xlsx uses).
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == "xl/workbook.xml") {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            p.1 = patch_sheet_names(&xml, &self.workbook.sheets).into_bytes();
        }
        true
    }

    pub fn remove_sheet(&mut self, idx: usize) -> bool {
        if self.workbook.sheets.len() <= 1 || idx >= self.workbook.sheets.len() {
            return false;
        }
        let dead: Vec<usize> = self
            .workbook
            .pivots
            .iter()
            .enumerate()
            .filter(|(_, p)| p.sheet == idx)
            .map(|(i, _)| i)
            .collect();
        for i in dead.into_iter().rev() {
            self.remove_pivot(i);
        }
        for p in &mut self.workbook.pivots {
            if p.sheet > idx {
                p.sheet -= 1;
            }
        }
        let part_name = self.sheet_parts.remove(idx);
        self.workbook.sheets.remove(idx);
        let active = &mut self.workbook.active_tab;
        if *active > idx || *active >= self.workbook.sheets.len() {
            *active = active.saturating_sub(1);
        }
        // The part, its rels, and what only it named (comments, drawings,
        // tables, printer settings), each with its content-type Override.
        drop_parts_cascading(&mut self.parts, &part_name);
        self.workbook.tables.retain(|t| t.sheet != idx);
        self.workbook
            .removed_tables
            .retain(|t| t.table.sheet != idx);
        let wb = &mut self.workbook;
        let removed = wb.removed_tables.iter_mut().map(|r| &mut r.table);
        for t in wb.tables.iter_mut().chain(removed) {
            if t.sheet > idx {
                t.sheet -= 1;
            }
        }
        // The workbook relationship whose target resolves to the part (a
        // relative, `./` or absolute Target alike) — capture its rId, then
        // remove it by that Id. The workbook part is wherever the package
        // rels put it, `xl/workbook.xml` only by convention.
        let wb_part = workbook_part_name(&self.parts);
        let wb_dir = wb_part.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let wb_rels = rels_part_name(&wb_part);
        let mut rid = String::new();
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == wb_rels) {
            let mut xml = String::from_utf8_lossy(&p.1).into_owned();
            if let Some((id, _, _)) = parse_rels(&xml)
                .into_iter()
                .find(|(_, _, t)| resolve_relative(wb_dir, t) == part_name)
            {
                if let Some(el) = find_element_by_attr(&xml, "Relationship", "Id", |v| v == id) {
                    xml.replace_range(el.start..el.end, "");
                }
                rid = id;
            }
            p.1 = xml.into_bytes();
        }
        // workbook.xml: drop the <sheet> element and fix defined-name scopes
        // (localSheetId counts sheets in document order).
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == wb_part) {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            let mut xml = if rid.is_empty() {
                xml
            } else {
                remove_element_containing(&xml, "<sheet ", &format!(":id=\"{rid}\""))
            };
            xml = shift_local_sheet_ids(&xml, idx);
            p.1 = xml.into_bytes();
        }
        self.workbook.defined_names.retain(|d| d.scope != Some(idx));
        for d in &mut self.workbook.defined_names {
            if let Some(s) = d.scope {
                if s > idx {
                    d.scope = Some(s - 1);
                }
            }
        }
        true
    }
}

/// Remove `part`, its own rels part, and, following those relationships, each
/// part that no remaining rels part names any more, with the content-type
/// Override of every part removed. A part still named elsewhere (an image
/// two drawings share) stays, and so does everything it names.
fn drop_parts_cascading(parts: &mut Vec<(String, Vec<u8>)>, part: &str) {
    let mut doomed = vec![part.to_string()];
    let mut removed: Vec<String> = Vec::new();
    while let Some(name) = doomed.pop() {
        if removed.contains(&name) {
            continue;
        }
        let own_rels = rels_part_name(&name);
        let dir = name.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let targets: Vec<String> = parts
            .iter()
            .find(|(n, _)| *n == own_rels)
            .map(|(_, xml)| {
                parse_rels(&String::from_utf8_lossy(xml))
                    .into_iter()
                    .map(|(_, _, t)| resolve_relative(dir, &t))
                    .collect()
            })
            .unwrap_or_default();
        parts.retain(|(n, _)| *n != name && *n != own_rels);
        removed.push(name);
        removed.push(own_rels);
        for t in targets {
            if parts.iter().any(|(n, _)| *n == t) && !part_is_named(parts, &t) {
                doomed.push(t);
            }
        }
    }
    if let Some(p) = parts.iter_mut().find(|(n, _)| n == "[Content_Types].xml") {
        let mut xml = String::from_utf8_lossy(&p.1).into_owned();
        for name in &removed {
            while let Some(el) = override_element(&xml, &format!("/{name}")) {
                xml.replace_range(el.start..el.end, "");
            }
        }
        p.1 = xml.into_bytes();
    }
}

/// Whether any rels part in `parts` has a relationship targeting `part`.
fn part_is_named(parts: &[(String, Vec<u8>)], part: &str) -> bool {
    parts.iter().any(|(n, xml)| {
        let Some((rels_dir, file)) = n.rsplit_once("_rels/") else {
            return false;
        };
        if !file.ends_with(".rels") {
            return false;
        }
        let dir = rels_dir.trim_end_matches('/');
        parse_rels(&String::from_utf8_lossy(xml))
            .iter()
            .any(|(_, _, t)| resolve_relative(dir, t) == part)
    })
}

/// Drop `<definedName localSheetId="removed">…</definedName>` elements and
/// decrement higher indices after a sheet removal.
fn shift_local_sheet_ids(xml: &str, removed: usize) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(pos) = rest.find("localSheetId=\"") {
        let vs = pos + "localSheetId=\"".len();
        let Some(ve) = rest[vs..].find('\"') else {
            break;
        };
        let digits = &rest[vs..vs + ve];
        match digits.parse::<usize>() {
            Ok(id) if id == removed => {
                // Remove the whole enclosing <definedName …>…</definedName>.
                if let Some(el_start) = rest[..pos].rfind("<definedName") {
                    let after = &rest[el_start..];
                    let el_end = after
                        .find("</definedName>")
                        .map(|i| i + "</definedName>".len())
                        .or_else(|| after.find("/>").map(|i| i + 2))
                        .unwrap_or(after.len());
                    out.push_str(&rest[..el_start]);
                    rest = &rest[el_start + el_end..];
                    continue;
                }
                out.push_str(&rest[..vs + ve]);
                rest = &rest[vs + ve..];
            }
            Ok(id) if id > removed => {
                out.push_str(&rest[..vs]);
                out.push_str(&(id - 1).to_string());
                rest = &rest[vs + ve..];
            }
            _ => {
                out.push_str(&rest[..vs + ve]);
                rest = &rest[vs + ve..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// New workbook
// ---------------------------------------------------------------------------

const SPREADSHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const RELS_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// Minimal but Excel-complete styles in the SpreadsheetML namespace `sml`:
/// two fills (none + gray125) are mandatory; one font, one border, one xf.
/// `dxfs` empty differential formats follow, so a repaired workbook's
/// conditional formats and tables keep a `dxfId` that resolves.
pub(crate) fn minimal_styles_xml(sml: &str, dxfs: usize) -> String {
    let dxfs = match dxfs {
        0 => String::new(),
        n => format!(r#"<dxfs count="{n}">{}</dxfs>"#, "<dxf/>".repeat(n)),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{sml}"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs><cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles>{dxfs}</styleSheet>"#
    )
}

/// A fresh single-sheet workbook (the "create new" path and a save target for
/// in-memory workbooks).
pub fn new_xlsx() -> SheetPackage {
    new_xlsx_sheets(&["Sheet1".to_string()])
}

/// A fresh workbook with one empty sheet per name, in order (at least one:
/// no names gives `Sheet1`). Every part is written in one pass, so it costs
/// time linear in the sheets, where [`SheetPackage::add_sheet`] rescans the
/// package for each.
pub(crate) fn new_xlsx_sheets(names: &[String]) -> SheetPackage {
    let fallback = ["Sheet1".to_string()];
    let names = if names.is_empty() {
        &fallback[..]
    } else {
        names
    };
    let n = names.len();
    let mut overrides = String::new();
    let mut entries = String::new();
    let mut sheet_rels = String::new();
    for (i, name) in names.iter().enumerate() {
        let k = i + 1;
        overrides.push_str(&format!(
            r#"<Override PartName="/xl/worksheets/sheet{k}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#
        ));
        entries.push_str(&format!(
            r#"<sheet name="{}" sheetId="{k}" r:id="rId{k}"/>"#,
            esc_attr(name)
        ));
        sheet_rels.push_str(&format!(
            r#"<Relationship Id="rId{k}" Type="{RELS_NS}/worksheet" Target="worksheets/sheet{k}.xml"/>"#
        ));
    }
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>{overrides}<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/></Types>"#
    );
    let root_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{RELS_NS}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#
    );
    let workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{SPREADSHEET_NS}" xmlns:r="{RELS_NS}"><sheets>{entries}</sheets></workbook>"#
    );
    let wb_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{sheet_rels}<Relationship Id="rId{}" Type="{RELS_NS}/styles" Target="styles.xml"/><Relationship Id="rId{}" Type="{RELS_NS}/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#,
        n + 1,
        n + 2
    );
    let worksheet = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{SPREADSHEET_NS}"><dimension ref="A1"/><sheetViews><sheetView workbookViewId="0"/></sheetViews><sheetData/></worksheet>"#
    );
    let styles = minimal_styles_xml(SPREADSHEET_NS, 0);
    let sst = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="0" uniqueCount="0"></sst>"#;

    let sheet_parts: Vec<String> = (1..=n)
        .map(|k| format!("xl/worksheets/sheet{k}.xml"))
        .collect();
    let mut parts = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types.into_bytes(),
        ),
        ("_rels/.rels".to_string(), root_rels.into_bytes()),
        ("xl/workbook.xml".to_string(), workbook.into_bytes()),
        (
            "xl/_rels/workbook.xml.rels".to_string(),
            wb_rels.into_bytes(),
        ),
    ];
    for part in &sheet_parts {
        parts.push((part.clone(), worksheet.as_bytes().to_vec()));
    }
    parts.push(("xl/styles.xml".to_string(), styles.into_bytes()));
    parts.push(("xl/sharedStrings.xml".to_string(), sst.as_bytes().to_vec()));

    SheetPackage {
        parts,
        sheet_parts,
        shared: Vec::new(),
        shared_part: Some("xl/sharedStrings.xml".to_string()),
        strict: false,
        workbook: Workbook {
            sheets: names
                .iter()
                .map(|name| Sheet {
                    name: name.clone(),
                    ..Sheet::default()
                })
                .collect(),
            styles: Styles {
                xfs: vec![Xf::default()],
                ..Default::default()
            },
            removed_tables: Vec::new(),
            defined_names: Vec::new(),
            tables: Vec::new(),
            pivots: Vec::new(),
            date1904: false,
            iterate: None,
            active_tab: 0,
        },
    }
}

#[cfg(test)]
mod tests {

    /// A line series' colour lives on its stroke, not its fill. Writing it as a
    /// fill leaves the line in Excel's default palette and tints the shape
    /// instead — silent visual damage to a chart the user only renamed.
    #[test]
    fn an_edited_line_chart_keeps_its_colour_on_the_stroke() {
        let series = |c: u32| crate::sheet::ChartSeries {
            name: "s".into(),
            values: vec![1.0, 2.0],
            color: Some(c),
            ..Default::default()
        };
        let line = crate::sheet::ChartData {
            title: "T".into(),
            kind: "line".into(),
            categories: vec!["a".into(), "b".into()],
            series: vec![series(0xC0705A)],
            ..Default::default()
        };
        let out = chart_space_xml(&line);
        assert!(
            out.contains("<a:ln><a:solidFill><a:srgbClr val=\"C0705A\"/></a:solidFill></a:ln>"),
            "a line's colour must be a stroke: {out}"
        );

        // Round-trips: the loader reads a line's colour from exactly there.
        assert_eq!(
            crate::drawing::parse_chart_for_test(&out).series[0].color,
            Some(0xC0705A)
        );

        // A column keeps the plain fill, which is where Excel looks for it.
        let col = crate::sheet::ChartData {
            kind: "column".into(),
            ..line.clone()
        };
        let out2 = chart_space_xml(&col);
        assert!(
            out2.contains(
                "<c:spPr><a:solidFill><a:srgbClr val=\"C0705A\"/></a:solidFill></c:spPr>"
            ),
            "a column's colour is a fill: {out2}"
        );
        assert_eq!(
            crate::drawing::parse_chart_for_test(&out2).series[0].color,
            Some(0xC0705A)
        );
    }

    /// An inserted chart must be addressable afterwards: its anchor has a real
    /// index in the host part, and its `part` names the chart XML we wrote. The
    /// first is what lets a delete remove it; the second is what lets an edit be
    /// regenerated. Both were missing, and both failed silently.
    #[test]
    fn an_inserted_chart_can_be_edited_and_deleted_afterwards() {
        let mut pkg = load_xlsx(&fixture()).expect("load");
        let data = crate::sheet::ChartData {
            title: "T".into(),
            kind: "column".into(),
            categories: vec!["a".into()],
            series: vec![crate::sheet::ChartSeries {
                name: "s".into(),
                values: vec![1.0],
                ..Default::default()
            }],
            ..Default::default()
        };
        pkg.add_chart(0, (0, 0), (10, 5), &data);

        let dw = pkg.workbook.sheets[0]
            .drawings
            .last()
            .expect("the chart is in the model");
        // It knows the part it was written to, so an edit reaches that part.
        let part = match &dw.kind {
            crate::sheet::DrawingKind::Chart(cd) => {
                cd.part.clone().expect("the chart part is recorded")
            }
            _ => panic!("expected a chart"),
        };
        assert!(
            pkg.part(&part).is_some(),
            "the recorded part exists in the package"
        );
        // And it knows where its anchor sits, rather than a sentinel.
        let dpart = pkg.workbook.sheets[0]
            .drawing_part
            .clone()
            .expect("a host part");
        let host = String::from_utf8_lossy(pkg.part(&dpart).unwrap()).into_owned();
        assert_eq!(
            dw.anchor_ix,
            crate::drawing::count_anchors(&host) - 1,
            "the anchor is the last one in the part"
        );

        // Deleting it must actually strike the anchor from the saved part.
        let ix = dw.anchor_ix;
        pkg.workbook.sheets[0].drawings.pop();
        pkg.workbook.sheets[0].drawings_removed.push(ix);
        let saved = load_xlsx(&save_xlsx(&pkg)).expect("reload after the delete");
        let host2 = String::from_utf8_lossy(saved.part(&dpart).unwrap()).into_owned();
        assert!(
            !host2.contains("Chart 1"),
            "the deleted chart's anchor is still in the part: {host2}"
        );
        assert!(
            saved.workbook.sheets[0]
                .drawings
                .iter()
                .all(|d| !matches!(d.kind, crate::sheet::DrawingKind::Chart(_))),
            "the chart came back on reopen"
        );
    }

    /// XML 1.0 forbids most C0 control characters outright — no escape can
    /// represent them — so a saved part carrying one is not well-formed and
    /// Excel rejects the whole workbook. Reachable from `=CHAR(1)`, and our own
    /// lenient loader reads it straight back, which is why round-trip tests
    /// never noticed.
    #[test]
    fn control_characters_never_reach_the_saved_package() {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, crate::sheet::Cell::text("a\u{1}b"));
        // A tab and a newline are legal and must survive; a carriage return is
        // legal but XML normalizes it away unless it is written as an entity.
        pkg.workbook.sheets[0].set_cell(1, 0, crate::sheet::Cell::text("x\ty\nz\r!"));
        let bytes = save_xlsx(&pkg);
        assert!(
            !bytes.windows(3).any(|w| w == [b'a', 0x01, b'b']),
            "the raw control byte reached the file"
        );

        // Reloading gives back everything XML can carry. (The zip's own headers
        // are binary, so the check has to be on the XML parts, not the archive.)
        let back = load_xlsx(&bytes).expect("reload");
        for (name, part) in back.parts.iter() {
            if name.ends_with(".xml") || name.ends_with(".rels") {
                assert!(
                    !part.iter().any(|&b| b < 0x20 && !matches!(b, 9 | 10 | 13)),
                    "{name} carries a raw control byte, so the part is not well-formed"
                );
            }
        }
        assert_eq!(
            back.workbook.sheets[0].cell(0, 0).map(|c| c.value.clone()),
            Some(crate::sheet::CellValue::Text("ab".into())),
            "the forbidden char is dropped, the rest is intact"
        );
        assert_eq!(
            back.workbook.sheets[0].cell(1, 0).map(|c| c.value.clone()),
            Some(crate::sheet::CellValue::Text("x\ty\nz\r!".into())),
            "tab, newline and carriage return all survive"
        );
    }

    /// Attribute values captured verbatim from the source file may legally hold
    /// `"` and `>` — they can come from a single-quoted attribute. Re-emitting
    /// them inside double quotes closes the element early and splices whatever
    /// follows into the worksheet as markup.
    #[test]
    fn preserved_raw_attributes_cannot_inject_markup() {
        // A well-formed worksheet whose row carries a single-quoted attribute
        // containing a quote and a tag: legal input, hostile on re-emission.
        let sheet1 = "<?xml version=\"1.0\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData><row r=\"1\" customFormat='x\"><injected/>'><c r=\"A1\"><v>7</v></c></row></sheetData></worksheet>";
        let workbook = "<?xml version=\"1.0\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"S\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>";
        let wb_rels = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>";
        let root_rels = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>";
        let content_types = "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/></Types>";
        let raw = write_zip(&[
            ("[Content_Types].xml".into(), content_types.into()),
            ("_rels/.rels".into(), root_rels.into()),
            ("xl/workbook.xml".into(), workbook.into()),
            ("xl/_rels/workbook.xml.rels".into(), wb_rels.into()),
            ("xl/worksheets/sheet1.xml".into(), sheet1.into()),
        ]);

        let pkg = load_xlsx(&raw).expect("the crafted workbook is well-formed and must load");
        assert!(
            pkg.workbook.sheets[0].row_attrs.contains_key(&0),
            "the row attribute was preserved"
        );
        let out = String::from_utf8_lossy(&save_xlsx(&pkg)).into_owned();
        assert!(
            !out.contains("<injected/>"),
            "attacker markup became part of the worksheet: {out}"
        );
        // The value itself survives, escaped, so the file still round-trips.
        assert!(
            out.contains("&quot;&gt;&lt;injected/&gt;"),
            "the raw value should be escaped, not dropped: {out}"
        );
    }
    use super::*;

    /// #707 r9 M1: a rule whose anchor a cleared rectangle moves takes its
    /// formulas along, in the model and through save and reload; an
    /// absolute reference stays.
    #[test]
    fn a_trimmed_rule_keeps_reading_its_own_cells() {
        let trimmed = |range, f: &str, cut| {
            let mut pkg = new_xlsx();
            assert!(pkg.add_data_validation(0, range, "custom", "", f, None));
            crate::edit::clear_validation(&mut pkg.workbook.sheets[0], cut);
            let dv = &pkg.workbook.sheets[0].validations[0];
            let model = (dv.ranges.clone(), dv.formula1.clone());
            let back = load_xlsx(&save_xlsx(&pkg)).expect("reload");
            let dv = &back.workbook.sheets[0].validations[0];
            assert_eq!(
                (dv.ranges.clone(), dv.formula1.clone()),
                model,
                "saved as held"
            );
            model
        };
        // The repro: A1>0 over A1:A10, A1:A3 cleared.
        assert_eq!(
            trimmed((0, 0, 9, 0), "A1>0", (0, 0, 2, 0)),
            (vec![(3, 0, 9, 0)], "A4>0".to_string())
        );
        // A horizontal rule losing its left column.
        assert_eq!(
            trimmed((0, 0, 0, 9), "A1>0", (0, 0, 0, 0)),
            (vec![(0, 1, 0, 9)], "B1>0".to_string())
        );
        // Absolute: unchanged.
        assert_eq!(
            trimmed((0, 0, 9, 0), "$A$1>0", (0, 0, 2, 0)),
            (vec![(3, 0, 9, 0)], "$A$1>0".to_string())
        );
        // A cut that leaves the anchor where it was changes no formula.
        assert_eq!(
            trimmed((0, 0, 9, 0), "A1>0", (5, 0, 6, 0)),
            (vec![(0, 0, 4, 0), (7, 0, 9, 0)], "A1>0".to_string())
        );
    }

    /// #707 r9: Clear Formats and Clear All take the conditional formatting
    /// off the cleared cells, a block whose anchor moved taking its formulas
    /// along, and one left with nothing going; saved and reloaded as held.
    /// Clear Contents leaves it.
    #[test]
    fn clearing_formats_trims_conditional_formatting() {
        use crate::edit::{ClearWhat, apply_clear_sheet, clear_plan};
        let cleared = |what, areas: &[(u32, u32, u32, u32)]| {
            let mut pkg = new_xlsx();
            assert!(pkg.add_conditional_format(
                0,
                (0, 0, 9, 0),
                "greaterThan",
                "B1",
                None,
                crate::sheet::Dxf::default()
            ));
            let plan = clear_plan(&pkg.workbook.sheets[0], areas, what, &[]).unwrap();
            apply_clear_sheet(&mut pkg.workbook.sheets[0], &plan);
            let held = |p: &SheetPackage| {
                p.workbook.sheets[0]
                    .cond_formats
                    .iter()
                    .map(|cf| (cf.ranges.clone(), cf.rules[0].formulas()[0].clone()))
                    .collect::<Vec<_>>()
            };
            let model = held(&pkg);
            let back = load_xlsx(&save_xlsx(&pkg)).expect("reload");
            assert_eq!(held(&back), model, "saved as held");
            model
        };
        assert_eq!(
            cleared(ClearWhat::Formats, &[(0, 0, 2, 0)]),
            vec![(vec![(3, 0, 9, 0)], "B4".to_string())]
        );
        assert_eq!(
            cleared(ClearWhat::All, &[(0, 0, 0, 0), (4, 0, 5, 0)]),
            vec![(vec![(1, 0, 3, 0), (6, 0, 9, 0)], "B2".to_string())]
        );
        assert!(cleared(ClearWhat::All, &[(0, 0, 20, 3)]).is_empty());
        assert_eq!(
            cleared(ClearWhat::Contents, &[(0, 0, 2, 0)]),
            vec![(vec![(0, 0, 9, 0)], "B1".to_string())]
        );
    }

    /// #707 r6: rules added in one batch read and save as one call each
    /// would, their ordinals claimed in order.
    #[test]
    fn add_data_validations_matches_one_call_per_rule() {
        let rules = [
            NewValidation {
                range: (0, 0, 4, 0),
                kind: "list",
                operator: "",
                formula1: "\"Laptop,Monitor,Dock\"",
                formula2: None,
                settings: None,
            },
            NewValidation {
                range: (0, 1, 4, 1),
                kind: "whole",
                operator: "between",
                formula1: "1",
                formula2: Some("10"),
                settings: None,
            },
        ];
        let mut one = new_xlsx();
        for r in &rules {
            assert!(
                one.add_data_validation(0, r.range, r.kind, r.operator, r.formula1, r.formula2)
            );
        }
        let mut batch = new_xlsx();
        assert!(batch.add_data_validations(0, &rules));
        let dvs = |p: &SheetPackage| format!("{:?}", p.workbook.sheets[0].validations);
        assert_eq!(dvs(&batch), dvs(&one));
        assert!(
            batch.workbook.sheets[0]
                .validations
                .iter()
                .map(|d| d.ix)
                .eq([Some(0), Some(1)])
        );
        assert_eq!(
            batch.part("xl/worksheets/sheet1.xml"),
            one.part("xl/worksheets/sheet1.xml")
        );
        let back = load_xlsx(&save_xlsx(&batch)).expect("reload");
        assert_eq!(back.workbook.sheets[0].validations.len(), 2);
    }

    #[test]
    fn add_data_validation_round_trips() {
        let mut pkg = new_xlsx();
        pkg.add_data_validation(0, (0, 0, 4, 0), "list", "", "\"Laptop,Monitor,Dock\"", None);
        pkg.add_data_validation(0, (0, 1, 4, 1), "whole", "between", "1", Some("10"));
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let dvs = &re.workbook.sheets[0].validations;
        assert_eq!(dvs.len(), 2);
        let list = dvs.iter().find(|d| d.kind == "list").unwrap();
        assert!(list.covers(2, 0));
        assert_eq!(list.formula1, "\"Laptop,Monitor,Dock\"");
        let whole = dvs.iter().find(|d| d.kind == "whole").unwrap();
        assert_eq!(whole.operator, "between");
        assert_eq!(
            (whole.formula1.as_str(), whole.formula2.as_str()),
            ("1", "10")
        );
    }

    #[test]
    fn add_conditional_format_round_trips_and_evaluates() {
        use crate::sheet::{Cell, Dxf};
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(100.0));
        pkg.workbook.sheets[0].set_cell(1, 0, Cell::number(900.0));
        // Highlight D-col > 500 with a red fill over A1:A2.
        let dxf = Dxf {
            fill: Some((255, 0, 0)),
            bold: Some(true),
            ..Dxf::default()
        };
        pkg.add_conditional_format(0, (0, 0, 1, 0), "greaterThan", "500", None, dxf);

        // Reload: the rule + dxf survive and evaluate.
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(re.workbook.sheets[0].cond_formats.len(), 1);
        assert!(!re.workbook.styles.dxfs.is_empty());
        // 100 doesn't match; 900 does (fill red + bold).
        assert!(crate::cf::cell_dxf(&re.workbook, 0, 0, 0).is_none());
        let d = crate::cf::cell_dxf(&re.workbook, 0, 1, 0).expect("900 > 500 should match");
        assert_eq!(d.fill, Some((255, 0, 0)));
        assert_eq!(d.bold, Some(true));
    }

    #[test]
    fn model_added_merges_round_trip() {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].merges.push((0, 0, 0, 3)); // A1:D1
        pkg.workbook.sheets[0].merges.push((2, 1, 4, 1)); // B3:B5
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(
            re.workbook.sheets[0].merges,
            vec![(0, 0, 0, 3), (2, 1, 4, 1)]
        );
        // Clearing them removes the block.
        let mut re = re;
        re.workbook.sheets[0].merges.clear();
        let re2 = load_xlsx(&save_xlsx(&re)).unwrap();
        assert!(re2.workbook.sheets[0].merges.is_empty());
        let ws =
            String::from_utf8(re2.part(&re2.sheet_parts[0].clone()).unwrap().to_vec()).unwrap();
        assert!(!ws.contains("<mergeCells"));
    }

    #[test]
    fn add_table_round_trips() {
        use crate::sheet::Cell;
        let mut pkg = new_xlsx();
        // Header row + two data rows over A1:B3.
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::text("Item"));
        pkg.workbook.sheets[0].set_cell(0, 1, Cell::text("Qty"));
        pkg.workbook.sheets[0].set_cell(1, 0, Cell::text("Pen"));
        pkg.workbook.sheets[0].set_cell(1, 1, Cell::number(3.0));
        let idx = pkg
            .add_table(0, (0, 0, 2, 1), true, "TableStyleMedium2")
            .unwrap();
        assert_eq!(pkg.workbook.tables[idx].columns, vec!["Item", "Qty"]);

        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(re.workbook.tables.len(), 1);
        let t = &re.workbook.tables[0];
        assert_eq!(t.name, "Table1");
        assert_eq!(t.range, (0, 0, 2, 1));
        assert_eq!(t.header_rows, 1);
        assert_eq!(t.columns, vec!["Item", "Qty"]);
        // The worksheet references the table via <tableParts>.
        let ws = String::from_utf8(re.part(&re.sheet_parts[0].clone()).unwrap().to_vec()).unwrap();
        assert!(
            ws.contains("<tableParts"),
            "worksheet must list the table part: {ws}"
        );
        assert!(ws.contains("r:id="), "tablePart needs an r:id");
    }

    #[test]
    fn add_table_dedupes_and_generates_column_names() {
        use crate::sheet::Cell;
        let mut pkg = new_xlsx();
        // Duplicate + blank headers must be uniquified / filled.
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::text("Name"));
        pkg.workbook.sheets[0].set_cell(0, 1, Cell::text("Name"));
        // C1 left blank.
        let idx = pkg
            .add_table(0, (0, 0, 1, 2), true, "TableStyleLight1")
            .unwrap();
        assert_eq!(
            pkg.workbook.tables[idx].columns,
            vec!["Name", "Name2", "Column3"]
        );
        // Without a header row, all columns are generated.
        let idx2 = pkg
            .add_table(0, (3, 0, 5, 1), false, "TableStyleLight1")
            .unwrap();
        assert_eq!(
            pkg.workbook.tables[idx2].columns,
            vec!["Column1", "Column2"]
        );
        assert_eq!(pkg.workbook.tables[idx2].name, "Table2");
    }

    #[test]
    fn row_outline_round_trips() {
        use crate::sheet::Cell;
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(1.0));
        pkg.workbook.sheets[0].set_row_outline(0, 2);
        pkg.workbook.sheets[0].set_row_hidden(0, true); // outline + hidden coexist
        assert_eq!(pkg.workbook.sheets[0].max_row_outline(), 2);
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(re.workbook.sheets[0].row_outline(0), 2);
        assert!(re.workbook.sheets[0].row_hidden(0));
        let ws = String::from_utf8(re.part(&re.sheet_parts[0].clone()).unwrap().to_vec()).unwrap();
        assert!(ws.contains("outlineLevel=\"2\""), "{ws}");
    }

    #[test]
    fn sheet_protection_round_trips() {
        let mut pkg = new_xlsx();
        assert!(!pkg.workbook.sheets[0].is_protected());
        pkg.workbook.sheets[0].set_protected(true);
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert!(re.workbook.sheets[0].is_protected());
        let ws = String::from_utf8(re.part(&re.sheet_parts[0].clone()).unwrap().to_vec()).unwrap();
        assert!(ws.contains("<sheetProtection sheet=\"1\""), "{ws}");
        // Unprotect → element gone.
        let mut re = re;
        re.workbook.sheets[0].set_protected(false);
        let re2 = load_xlsx(&save_xlsx(&re)).unwrap();
        assert!(!re2.workbook.sheets[0].is_protected());
        let ws2 =
            String::from_utf8(re2.part(&re2.sheet_parts[0].clone()).unwrap().to_vec()).unwrap();
        assert!(!ws2.contains("<sheetProtection"));
    }

    #[test]
    fn sheet_protection_preserves_existing_attrs_and_order() {
        // A pre-existing protection element with a password + custom flags must
        // survive verbatim, and land before <mergeCells>.
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].protection =
            Some("sheet=\"1\" password=\"CC3D\" formatCells=\"0\"".into());
        pkg.workbook.sheets[0].merges.push((0, 0, 0, 2));
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(
            re.workbook.sheets[0].protection.as_deref(),
            Some("sheet=\"1\" password=\"CC3D\" formatCells=\"0\"")
        );
        let ws = String::from_utf8(re.part(&re.sheet_parts[0].clone()).unwrap().to_vec()).unwrap();
        let (pp, mp) = (
            ws.find("<sheetProtection").unwrap(),
            ws.find("<mergeCells").unwrap(),
        );
        assert!(pp < mp, "sheetProtection must precede mergeCells: {ws}");
    }

    #[test]
    fn freeze_round_trips_through_save() {
        // Freeze set on the model must serialize to <pane> and reload identically,
        // for both a template sheet (self-closing <sheetView/>) and an added sheet
        // (no <sheetViews> at all).
        let mut pkg = new_xlsx();
        let added = pkg.add_sheet("Two");
        pkg.workbook.sheets[0].freeze = (2, 3);
        pkg.workbook.sheets[added].freeze = (1, 0);
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(re.workbook.sheets[0].freeze, (2, 3));
        assert_eq!(re.workbook.sheets[added].freeze, (1, 0));
        // Unfreeze then save again → pane gone, freeze reads (0,0).
        let mut re = re;
        re.workbook.sheets[0].freeze = (0, 0);
        let re2 = load_xlsx(&save_xlsx(&re)).unwrap();
        assert_eq!(re2.workbook.sheets[0].freeze, (0, 0));
        let ws0 =
            String::from_utf8(re2.part(&re2.sheet_parts[0].clone()).unwrap().to_vec()).unwrap();
        assert!(
            !ws0.contains("<pane"),
            "unfreeze should remove the pane: {ws0}"
        );
    }

    #[test]
    fn rename_sheet_updates_model_and_workbook_xml() {
        let mut pkg = new_xlsx();
        let s2 = pkg.add_sheet("Data");
        assert_eq!(pkg.workbook.sheets[s2].name, "Data");
        // Rename succeeds and updates both the model and workbook.xml.
        assert!(pkg.rename_sheet(s2, "Budget"));
        assert_eq!(pkg.workbook.sheets[s2].name, "Budget");
        let wbxml = String::from_utf8(pkg.part("xl/workbook.xml").unwrap().to_vec()).unwrap();
        assert!(
            wbxml.contains("name=\"Budget\""),
            "workbook.xml not updated: {wbxml}"
        );
        assert!(!wbxml.contains("name=\"Data\""));
        // Duplicate (case-insensitive) and empty names are rejected.
        let first = pkg.workbook.sheets[0].name.clone();
        assert!(!pkg.rename_sheet(s2, &first.to_uppercase()));
        assert!(!pkg.rename_sheet(s2, "   "));
        assert_eq!(pkg.workbook.sheets[s2].name, "Budget");
        // Survives a save→load round-trip.
        let bytes = save_xlsx(&pkg);
        let re = load_xlsx(&bytes).unwrap();
        assert!(re.workbook.sheets.iter().any(|s| s.name == "Budget"));
    }

    #[test]
    fn edited_chart_series_refs_round_trip_through_save_and_load() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource, DrawingKind};
        let src = |range| ChartSource {
            sheet: "Sheet1".into(),
            range,
            cat_col: 0,
        };
        let mut pkg = new_xlsx();
        // Two series reading different columns, plus their own category labels.
        let data = ChartData {
            title: "Sales".into(),
            kind: "column".into(),
            categories: vec!["Laptop".into(), "Dock".into()],
            series: vec![
                ChartSeries {
                    name: "Qty".into(),
                    values: vec![2.0, 5.0],
                    col: Some(1),
                    values_ref: Some(src((1, 1, 2, 1))),
                    name_ref: Some("Sheet1!$B$1".into()),
                    ..Default::default()
                },
                ChartSeries {
                    name: "Total".into(),
                    values: vec![2398.0, 358.0],
                    col: Some(3),
                    values_ref: Some(src((1, 3, 2, 3))),
                    name_ref: Some("Sheet1!$D$1".into()),
                    ..Default::default()
                },
            ],
            source: Some(src((0, 0, 2, 3))),
            categories_ref: Some(src((1, 0, 2, 0))),
            ..Default::default()
        };
        pkg.add_chart(0, (5, 0), (20, 8), &data);

        // Save the package and read it back the way opening the file would.
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let chart = |p: &SheetPackage| match &p.workbook.sheets[0]
            .drawings
            .first()
            .expect("chart drawing")
            .kind
        {
            DrawingKind::Chart(c) => c.clone(),
            other => panic!("expected a chart drawing, got {other:?}"),
        };
        let got = chart(&re);
        assert_eq!(got.series.len(), 2);
        assert_eq!(
            got.series[0].values_ref.as_ref().map(|v| v.range),
            Some((1, 1, 2, 1))
        );
        assert_eq!(
            got.series[1].values_ref.as_ref().map(|v| v.range),
            Some((1, 3, 2, 3))
        );
        assert_eq!(got.series[0].name_ref.as_deref(), Some("Sheet1!$B$1"));
        assert_eq!(got.series[1].name_ref.as_deref(), Some("Sheet1!$D$1"));
        assert_eq!(
            got.categories_ref.as_ref().map(|v| v.range),
            Some((1, 0, 2, 0))
        );
        assert_eq!(got.series[1].values, vec![2398.0, 358.0]);
        assert_eq!(got.categories, vec!["Laptop", "Dock"]);
        // The loader knows which part to write an edit back into.
        assert_eq!(got.part.as_deref(), Some("xl/charts/chart1.xml"));

        // Now edit one series the way the panel does — re-point it at another
        // column, rename it, and move the categories — and save again. Only an
        // `edited` chart is regenerated, so this is the path that matters.
        let mut edited = re;
        let mut cd = got;
        cd.series[1].values_ref = Some(src((1, 2, 2, 2)));
        cd.series[1].values = vec![1199.0, 179.0];
        cd.series[1].col = Some(2);
        cd.series[1].name = "Unit price".into();
        cd.series[1].name_ref = Some("Sheet1!$C$1".into());
        cd.categories_ref = Some(src((1, 4, 2, 4)));
        cd.edited = true;
        edited.workbook.sheets[0].drawings[0].kind = DrawingKind::Chart(cd);

        let reopened = chart(&load_xlsx(&save_xlsx(&edited)).unwrap());
        // The edit survived the trip to disk…
        assert_eq!(
            reopened.series[1].values_ref.as_ref().map(|v| v.range),
            Some((1, 2, 2, 2))
        );
        assert_eq!(reopened.series[1].name_ref.as_deref(), Some("Sheet1!$C$1"));
        assert_eq!(reopened.series[1].name, "Unit price");
        assert_eq!(reopened.series[1].values, vec![1199.0, 179.0]);
        assert_eq!(
            reopened.categories_ref.as_ref().map(|v| v.range),
            Some((1, 4, 2, 4))
        );
        // …and the series that was left alone came back untouched.
        assert_eq!(
            reopened.series[0].values_ref.as_ref().map(|v| v.range),
            Some((1, 1, 2, 1))
        );
        assert_eq!(reopened.series[0].name_ref.as_deref(), Some("Sheet1!$B$1"));
        assert_eq!(reopened.series[0].values, vec![2.0, 5.0]);
    }

    #[test]
    fn a_re_pointed_series_caches_only_the_cells_its_own_ref_names() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource, DrawingKind};
        let src = |range| ChartSource {
            sheet: "Sheet1".into(),
            range,
            cat_col: 0,
        };
        // Series are re-pointed ONE at a time, so their lengths diverge. A
        // chart-wide point count would pad the short one with zeros and write
        // that count beside its own three-cell `<c:f>` — a cache contradicting
        // its ref, and six phantom bars when the file is read back.
        let data = ChartData {
            kind: "column".into(),
            categories: vec!["Jan".into(), "Feb".into(), "Mar".into()],
            series: vec![
                ChartSeries {
                    name: "Short".into(),
                    values: vec![9.0, 8.0, 7.0],
                    col: Some(1),
                    values_ref: Some(src((1, 1, 3, 1))),
                    ..Default::default()
                },
                ChartSeries {
                    name: "Long".into(),
                    values: vec![1.0, 2.0, 3.0, 4.0, 5.0],
                    col: Some(2),
                    values_ref: Some(src((1, 2, 5, 2))),
                    ..Default::default()
                },
            ],
            source: Some(src((0, 0, 5, 2))),
            categories_ref: Some(src((1, 0, 3, 0))),
            edited: true,
            ..Default::default()
        };
        let xml = chart_space_xml(&data);
        assert!(
            xml.contains("<c:ptCount val=\"3\"/>") && xml.contains("<c:ptCount val=\"5\"/>"),
            "each cache is sized from its own slot: {xml}"
        );
        // Read it back the way opening the file would.
        let mut pkg = new_xlsx();
        pkg.add_chart(0, (5, 0), (20, 8), &data);
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        match &re.workbook.sheets[0].drawings[0].kind {
            DrawingKind::Chart(c) => {
                assert_eq!(c.series[0].values, vec![9.0, 8.0, 7.0], "no padding");
                assert_eq!(c.series[1].values, vec![1.0, 2.0, 3.0, 4.0, 5.0]);
                assert_eq!(c.categories, vec!["Jan", "Feb", "Mar"]);
            }
            other => panic!("expected a chart, got {other:?}"),
        }
    }

    #[test]
    fn adding_a_chart_never_moves_an_anchor_in_the_loaded_drawing_part() {
        use crate::sheet::{ChartData, ChartSeries, DrawingKind};
        // A sheet whose drawing part holds ONE anchor we don't model (a shape).
        // `parse_drawings` therefore yields no Drawing, but `drawing_part` is
        // still set, so a save runs the anchor rewrite over it.
        let mut pkg = new_xlsx();
        let dpart = "xl/drawings/drawing1.xml";
        let shape = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><xdr:twoCellAnchor><xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>3</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>4</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:sp/><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>"#;
        pkg.parts
            .push((dpart.to_string(), shape.as_bytes().to_vec()));
        pkg.workbook.sheets[0].drawing_part = Some(dpart.to_string());
        assert!(
            pkg.workbook.sheets[0].drawings.is_empty(),
            "the shape is not modelled"
        );

        let data = ChartData {
            title: "Sales".into(),
            kind: "column".into(),
            categories: vec!["Q1".into()],
            series: vec![ChartSeries {
                name: "East".into(),
                values: vec![1.0],
                ..Default::default()
            }],
            ..Default::default()
        };
        // The chart lands far from the shape, and joins the part the sheet
        // already has — a worksheet can name only one, so a part of its own
        // would be orphaned the moment Excel opened the file.
        pkg.add_chart(0, (20, 5), (35, 12), &data);
        let saved = save_xlsx(&pkg);
        let re = load_xlsx(&saved).unwrap();
        let out =
            String::from_utf8(re.part(dpart).expect("the old drawing part").to_vec()).unwrap();
        let head = shape.trim_end_matches("</xdr:wsDr>");
        assert!(
            out.starts_with(head),
            "the shape's anchor must come back byte for byte, got:\n{out}"
        );
        assert!(
            out[head.len()..].contains("<xdr:row>20</xdr:row>"),
            "the chart's anchor is appended after it"
        );
        assert!(
            re.part("xl/drawings/drawing2.xml").is_none(),
            "no second drawing part — the worksheet could not reference it"
        );
        let ws = String::from_utf8(re.part("xl/worksheets/sheet1.xml").unwrap().to_vec()).unwrap();
        assert_eq!(ws.matches("<drawing ").count(), 1);

        // And it comes back as a chart on that sheet.
        assert_eq!(re.workbook.sheets[0].drawings.len(), 1);
        assert!(matches!(
            re.workbook.sheets[0].drawings[0].kind,
            DrawingKind::Chart(_)
        ));
        assert_eq!(re.workbook.sheets[0].drawings[0].from, (20, 5));

        // Striking an anchor the sheet was loaded with must not disturb the
        // shape either (an authored chart owns no index in the part, so its
        // `drawings_removed` entry can't name a stranger's anchor).
        let mut deleted = pkg.clone();
        let gone = deleted.workbook.sheets[0]
            .drawings
            .pop()
            .expect("the chart we just added");
        assert!(matches!(gone.kind, DrawingKind::Chart(_)));
        deleted.workbook.sheets[0]
            .drawings_removed
            .push(gone.anchor_ix);
        let re = load_xlsx(&save_xlsx(&deleted)).unwrap();
        let out =
            String::from_utf8(re.part(dpart).expect("the old drawing part").to_vec()).unwrap();
        assert!(
            out.starts_with(head),
            "the shape survives a delete, got:\n{out}"
        );
    }

    #[test]
    fn two_authored_charts_on_a_sheet_both_survive_a_save() {
        use crate::sheet::{ChartData, ChartSeries, DrawingKind};
        // Each chart used to mint a drawing part of its own, but a worksheet may
        // reference only ONE — so every chart after the first was orphaned in
        // the saved file (Excel showed the first, we showed the last).
        let mut pkg = new_xlsx();
        let data = |title: &str| ChartData {
            title: title.into(),
            kind: "column".into(),
            categories: vec!["Q1".into()],
            series: vec![ChartSeries {
                name: "East".into(),
                values: vec![1.0],
                ..Default::default()
            }],
            ..Default::default()
        };
        pkg.add_chart(0, (1, 1), (10, 6), &data("First"));
        pkg.add_chart(0, (20, 1), (30, 6), &data("Second"));

        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let titles: Vec<String> = re.workbook.sheets[0]
            .drawings
            .iter()
            .filter_map(|d| match &d.kind {
                DrawingKind::Chart(cd) => Some(cd.title.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(titles, vec!["First".to_string(), "Second".to_string()]);
        assert!(re.part("xl/drawings/drawing2.xml").is_none());
        let ws = String::from_utf8(re.part("xl/worksheets/sheet1.xml").unwrap().to_vec()).unwrap();
        assert_eq!(ws.matches("<drawing ").count(), 1);
    }

    #[test]
    fn chart_space_xml_per_kind() {
        use crate::sheet::{ChartData, ChartSeries};
        let data = |kind: &str| ChartData {
            title: "Sales".into(),
            kind: kind.into(),
            categories: vec!["Q1".into(), "Q2".into(), "Q3".into()],
            series: vec![
                ChartSeries {
                    name: "East".into(),
                    values: vec![1.0, 2.0, 3.0],
                    ..Default::default()
                },
                ChartSeries {
                    name: "West".into(),
                    values: vec![4.0, 5.0, 6.0],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        // Well-formedness proxy: equal open/close angle brackets and matched
        // <c:chartSpace>…</c:chartSpace>, plus the expected plot element per kind.
        for (kind, needle, forbidden) in [
            ("column", "<c:barChart><c:barDir val=\"col\"/>", "<c:catAx"),
            ("bar", "<c:barChart><c:barDir val=\"bar\"/>", "<c:catAx"),
            ("line", "<c:lineChart>", "<c:catAx"),
            ("pie", "<c:pieChart>", "<c:catAx"),
        ] {
            let xml = chart_space_xml(&data(kind));
            assert!(xml.contains(needle), "{kind}: missing {needle}");
            assert!(
                xml.matches('<').count() == xml.matches('>').count(),
                "{kind}: unbalanced angle brackets"
            );
            assert!(
                xml.matches("<c:chartSpace").count() == 1 && xml.contains("</c:chartSpace>"),
                "{kind}: chartSpace not closed"
            );
            // Pie carries no axes; the axed kinds must include the shared catAx.
            // Every kind writes every series, pie included.
            if kind == "pie" {
                assert!(!xml.contains(forbidden), "pie must not emit axes");
            } else {
                assert!(xml.contains(forbidden), "{kind}: missing axes");
            }
            assert_eq!(
                xml.matches("<c:ser>").count(),
                2,
                "{kind}: both series expected"
            );
        }
    }

    #[test]
    fn a_pie_writes_every_series_it_holds() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource};
        // ECMA-376 lets a `<c:pieChart>` hold several `<c:ser>`; Excel plots
        // the first. The writer used to emit only that one, which DELETED the
        // rest of the user's data on save.
        let ser = |name: &str, col: u32, v: f64| ChartSeries {
            name: name.into(),
            values: vec![v, v + 1.0],
            col: Some(col),
            values_ref: Some(ChartSource {
                sheet: "Budget".into(),
                range: (1, col, 2, col),
                cat_col: 0,
            }),
            ..Default::default()
        };
        let data = ChartData {
            title: "Sales".into(),
            kind: "pie".into(),
            categories: vec!["Q1".into(), "Q2".into()],
            series: vec![
                ser("East", 1, 1.0),
                ser("West", 2, 3.0),
                ser("North", 3, 5.0),
            ],
            ..Default::default()
        };
        let xml = chart_space_xml(&data);
        assert_eq!(xml.matches("<c:ser>").count(), 3, "all three are written");
        // Each names its own cells — three copies of one ref would be a
        // different kind of loss.
        for f in ["Budget!$B$2:$B$3", "Budget!$C$2:$C$3", "Budget!$D$2:$D$3"] {
            assert!(xml.contains(f), "missing values ref {f}");
        }
        for n in ["East", "West", "North"] {
            assert!(xml.contains(n), "missing series name {n}");
        }
        // `<c:idx>`/`<c:order>` run 0,1,2 the way the axed arms produce them.
        for i in 0..3 {
            assert!(
                xml.contains(&format!("<c:idx val=\"{i}\"/><c:order val=\"{i}\"/>")),
                "series {i} is not indexed sequentially"
            );
        }
        assert!(!xml.contains("<c:catAx"), "a pie still carries no axes");
        assert_eq!(xml.matches('<').count(), xml.matches('>').count());
    }

    #[test]
    fn a_one_series_pie_is_written_exactly_as_before() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource};
        // The common case must not move: one series in, one `<c:ser>` out,
        // wrapped in the same `<c:pieChart>` with the same varyColors and
        // firstSliceAng and no axes.
        let data = ChartData {
            title: "Sales".into(),
            kind: "pie".into(),
            categories: vec!["Q1".into(), "Q2".into()],
            series: vec![ChartSeries {
                name: "East".into(),
                values: vec![1.0, 2.0],
                col: Some(1),
                values_ref: Some(ChartSource {
                    sheet: "Budget".into(),
                    range: (1, 1, 2, 1),
                    cat_col: 0,
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let xml = chart_space_xml(&data);
        assert_eq!(xml.matches("<c:ser>").count(), 1);
        let plot = &xml[xml.find("<c:pieChart>").unwrap()..xml.find("</c:pieChart>").unwrap()];
        assert!(plot.starts_with("<c:pieChart><c:varyColors val=\"1\"/><c:ser>"));
        assert!(plot.ends_with("</c:ser><c:firstSliceAng val=\"0\"/>"));
        assert!(!xml.contains("<c:catAx") && !xml.contains("<c:valAx"));
        // And it is byte-identical to what the same series produced when the
        // arm wrote `series.first()`: that is exactly `ser_xml(0, s)`, which is
        // what a one-element `{sers}` still is.
        assert!(xml.contains("<c:idx val=\"0\"/><c:order val=\"0\"/>"));
        assert!(xml.contains("Budget!$B$2:$B$3"));
    }

    /// The write half of the fix is only half of it: a save that emits three
    /// `<c:ser>` still loses two series if the loader that reads the file back
    /// keeps one. `parse_chart` walks `<c:ser>` generically, so this should
    /// already hold — pinned rather than assumed, because it is the assertion
    /// that actually says "the user's work survived the save".
    #[test]
    fn a_three_series_pie_survives_a_write_and_a_read() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource};
        let ser = |name: &str, col: u32, v: f64| ChartSeries {
            name: name.into(),
            values: vec![v, v + 1.0],
            col: Some(col),
            values_ref: Some(ChartSource {
                sheet: "Budget".into(),
                range: (1, col, 2, col),
                cat_col: 0,
            }),
            ..Default::default()
        };
        let data = ChartData {
            title: "Sales".into(),
            kind: "pie".into(),
            categories: vec!["Q1".into(), "Q2".into()],
            series: vec![
                ser("East", 1, 1.0),
                ser("West", 2, 3.0),
                ser("North", 3, 5.0),
            ],
            ..Default::default()
        };
        let back = crate::drawing::parse_chart_for_test(&chart_space_xml(&data));
        assert_eq!(back.kind, "pie");
        assert_eq!(back.series.len(), 3, "all three came back");
        // Names, cached values and the cells each series reads — a series that
        // returns nameless or pointed at another series' column is the same
        // loss wearing a different shape.
        for (i, (name, col, v)) in [("East", 1, 1.0), ("West", 2, 3.0), ("North", 3, 5.0)]
            .into_iter()
            .enumerate()
        {
            let s = &back.series[i];
            assert_eq!(s.name, name, "series {i} name");
            assert_eq!(s.values, vec![v, v + 1.0], "series {i} values");
            assert_eq!(
                s.values_ref.as_ref().map(ChartSource::to_ref),
                Some(format!(
                    "Budget!${}$2:${}$3",
                    (b'A' + col as u8) as char,
                    (b'A' + col as u8) as char
                )),
                "series {i} values ref"
            );
        }
        assert_eq!(back.categories, vec!["Q1".to_string(), "Q2".to_string()]);
        // And it stays editable, so the NEXT save regenerates rather than
        // copying a part the writer was assumed not to understand.
        assert!(!back.complex, "a multi-series pie is not held back");
        assert!(chart_is_writable(&back));
        // Writing what came back reproduces the same part: the round trip is
        // stable, not merely lossless once.
        assert_eq!(chart_space_xml(&back), chart_space_xml(&data));
    }

    /// The common case, end to end. One series in, one series out, unchanged —
    /// the multi-series arm must not have cost the single-series pie anything.
    #[test]
    fn a_one_series_pie_survives_a_write_and_a_read() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource};
        let data = ChartData {
            title: "Sales".into(),
            kind: "pie".into(),
            categories: vec!["Q1".into(), "Q2".into()],
            series: vec![ChartSeries {
                name: "East".into(),
                values: vec![1.0, 2.0],
                col: Some(1),
                values_ref: Some(ChartSource {
                    sheet: "Budget".into(),
                    range: (1, 1, 2, 1),
                    cat_col: 0,
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let back = crate::drawing::parse_chart_for_test(&chart_space_xml(&data));
        assert_eq!(back.kind, "pie");
        assert_eq!(back.series.len(), 1);
        assert_eq!(back.series[0].name, "East");
        assert_eq!(back.series[0].values, vec![1.0, 2.0]);
        assert_eq!(
            back.series[0].values_ref.as_ref().map(ChartSource::to_ref),
            Some("Budget!$B$2:$B$3".to_string())
        );
        assert_eq!(back.categories, vec!["Q1".to_string(), "Q2".to_string()]);
        assert!(!back.complex);
        assert_eq!(chart_space_xml(&back), chart_space_xml(&data));
    }

    /// What the suite's "+ Series" button now does to a pie, and what used to
    /// be refused at that button because the save undid it.
    ///
    /// `series_add` (suite/docxy/src/main.rs) pushes a series with a default
    /// name, one zero per category, and no `values_ref` yet — the user points
    /// it at cells next. The refusal was there because `chart_space_xml` wrote
    /// only `series.first()`, so the pushed series was listed in the panel,
    /// pointed at cells, coloured, and then gone from the file. The button is
    /// open now; this is the assertion that says opening it costs nothing.
    #[test]
    fn a_series_added_to_a_pie_survives_a_save() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource};
        let mut data = ChartData {
            title: "Sales".into(),
            kind: "pie".into(),
            categories: vec!["Q1".into(), "Q2".into()],
            series: vec![ChartSeries {
                name: "East".into(),
                values: vec![1.0, 2.0],
                col: Some(1),
                values_ref: Some(ChartSource {
                    sheet: "Budget".into(),
                    range: (1, 1, 2, 1),
                    cat_col: 0,
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        // The push, exactly as the button makes it.
        data.series.push(ChartSeries {
            name: "Series 2".into(),
            values: vec![0.0; data.categories.len()],
            ..Default::default()
        });

        let xml = chart_space_xml(&data);
        assert_eq!(xml.matches("<c:ser>").count(), 2, "both series written");
        let back = crate::drawing::parse_chart_for_test(&xml);
        assert_eq!(back.kind, "pie");
        assert_eq!(back.series.len(), 2, "the added series came back");
        assert_eq!(back.series[0].name, "East");
        assert_eq!(back.series[0].values, vec![1.0, 2.0]);
        // The fresh one keeps its name and its (empty) numbers, so the user can
        // go on pointing it at cells after a save and a reload.
        assert_eq!(back.series[1].name, "Series 2");
        assert_eq!(back.series[1].values, vec![0.0, 0.0]);
        assert!(!back.complex, "still editable");
        assert!(chart_is_writable(&back));

        // Pointing it at cells afterwards is the other half of the same story:
        // the ref rides the next save out and back too.
        let mut pointed = back.clone();
        pointed.series[1].values = vec![3.0, 4.0];
        pointed.series[1].col = Some(2);
        pointed.series[1].values_ref = Some(ChartSource {
            sheet: "Budget".into(),
            range: (1, 2, 2, 2),
            cat_col: 0,
        });
        let again = crate::drawing::parse_chart_for_test(&chart_space_xml(&pointed));
        assert_eq!(again.series.len(), 2);
        assert_eq!(
            again.series[1].values_ref.as_ref().map(ChartSource::to_ref),
            Some("Budget!$C$2:$C$3".to_string())
        );
        assert_eq!(again.series[1].values, vec![3.0, 4.0]);
    }

    /// The Overview's scenario, end to end through a real file: three series,
    /// pick **Pie**, save, reopen. Before this plan the click was refused; with
    /// the refusal gone the click lands, and this is the assertion that the
    /// save it leads to no longer eats two thirds of the chart.
    ///
    /// Deliberately at PACKAGE level rather than through `chart_space_xml`
    /// alone: the regeneration is gated on `edited && chart_is_writable &&
    /// part`, the part is rewritten inside the zip, and the reopen goes back
    /// through `load_xlsx`. A test that stopped at the XML string would pass
    /// while any one of those three dropped the chart on the floor.
    #[test]
    fn three_series_clicked_to_pie_survive_a_save_and_a_reopen() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource, DrawingKind};
        let src = |range| ChartSource {
            sheet: "Sheet1".into(),
            range,
            cat_col: 0,
        };
        let ser = |name: &str, col: u32, v: f64| ChartSeries {
            name: name.into(),
            values: vec![v, v + 1.0],
            col: Some(col),
            values_ref: Some(src((1, col, 2, col))),
            name_ref: Some(format!("Sheet1!${}$1", (b'A' + col as u8) as char)),
            ..Default::default()
        };
        let chart = |p: &SheetPackage| match &p.workbook.sheets[0]
            .drawings
            .first()
            .expect("chart drawing")
            .kind
        {
            DrawingKind::Chart(c) => c.clone(),
            other => panic!("expected a chart drawing, got {other:?}"),
        };
        let mut pkg = new_xlsx();
        // What the user has on screen before the click: an ordinary column
        // chart over three columns, each series pointed at its own cells.
        pkg.add_chart(
            0,
            (5, 0),
            (20, 8),
            &ChartData {
                title: "Sales".into(),
                kind: "column".into(),
                categories: vec!["Q1".into(), "Q2".into()],
                series: vec![
                    ser("East", 1, 1.0),
                    ser("West", 2, 3.0),
                    ser("North", 3, 5.0),
                ],
                source: Some(src((0, 0, 2, 3))),
                categories_ref: Some(src((1, 0, 2, 0))),
                ..Default::default()
            },
        );
        let mut pkg = load_xlsx(&save_xlsx(&pkg)).expect("the column chart reopens");
        assert_eq!(chart(&pkg).series.len(), 3);

        // The click: `chart_take_kind` (suite) relabels the chart, clears
        // `complex` and — the kind being writable — the per-series point slots,
        // and committing it marks the chart edited. The point slots are empty
        // on a chart the writer itself produced, so the two lines below are the
        // whole of what that call does here.
        let mut cd = chart(&pkg);
        cd.kind = "pie".into();
        cd.complex = false;
        cd.edited = true;
        pkg.workbook.sheets[0].drawings[0].kind = DrawingKind::Chart(cd);

        // Save…
        let bytes = save_xlsx(&pkg);
        // …and the part that went to disk really is a pie holding all three,
        // not a pie holding one. Asserted on the saved bytes because that is
        // what Excel would be handed.
        let reopened = load_xlsx(&bytes).expect("the pie reopens");
        let part = String::from_utf8(
            reopened
                .part("xl/charts/chart1.xml")
                .expect("the chart part")
                .to_vec(),
        )
        .expect("utf-8");
        let plot = &part[part.find("<c:pieChart>").expect("a pie was written")
            ..part.find("</c:pieChart>").expect("closed")];
        assert_eq!(plot.matches("<c:ser>").count(), 3, "three slice groups");

        // …reopen.
        let back = chart(&reopened);
        assert_eq!(back.kind, "pie");
        assert_eq!(back.series.len(), 3, "all three series are still there");
        for (i, (name, col, v)) in [("East", 1, 1.0), ("West", 2, 3.0), ("North", 3, 5.0)]
            .into_iter()
            .enumerate()
        {
            let s = &back.series[i];
            assert_eq!(s.name, name, "series {i} name");
            assert_eq!(s.values, vec![v, v + 1.0], "series {i} values");
            assert_eq!(
                s.values_ref.as_ref().map(|r| r.range),
                Some((1, col, 2, col)),
                "series {i} still reads its own cells"
            );
        }
        assert_eq!(back.categories, vec!["Q1".to_string(), "Q2".to_string()]);
        // Reopened editable rather than held back, so the next save regenerates
        // — the `complex` hold-back Task 2 removed does not creep back in by
        // way of the file.
        assert!(!back.complex, "editable on reopen");
        assert!(chart_is_writable(&back));
        // Only the first is DRAWN. The suite says that in its own crate
        // (`chart_plotted_series`, `chart_unplotted_note`); what the file half
        // owes is that the other two are present to be listed at all.
    }

    /// Pie → Column → Pie is lossless. The conversion is why the model must not
    /// cap a pie at one series (see "Why not enforce it in the model" in the
    /// plan): a cap would make picking Pie destroy the very data that picking
    /// Column back is supposed to return.
    #[test]
    fn a_pie_converted_to_column_and_back_keeps_every_series() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource, DrawingKind};
        let src = |range| ChartSource {
            sheet: "Sheet1".into(),
            range,
            cat_col: 0,
        };
        let ser = |name: &str, col: u32, v: f64| ChartSeries {
            name: name.into(),
            values: vec![v, v + 1.0],
            col: Some(col),
            values_ref: Some(src((1, col, 2, col))),
            ..Default::default()
        };
        let chart = |p: &SheetPackage| match &p.workbook.sheets[0]
            .drawings
            .first()
            .expect("chart drawing")
            .kind
        {
            DrawingKind::Chart(c) => c.clone(),
            other => panic!("expected a chart drawing, got {other:?}"),
        };
        let mut pkg = new_xlsx();
        pkg.add_chart(
            0,
            (5, 0),
            (20, 8),
            &ChartData {
                title: "Sales".into(),
                kind: "pie".into(),
                categories: vec!["Q1".into(), "Q2".into()],
                series: vec![
                    ser("East", 1, 1.0),
                    ser("West", 2, 3.0),
                    ser("North", 3, 5.0),
                ],
                source: Some(src((0, 0, 2, 3))),
                categories_ref: Some(src((1, 0, 2, 0))),
                ..Default::default()
            },
        );
        // Round-trip once as a pie, then relabel to column, save, reopen.
        let mut pkg = load_xlsx(&save_xlsx(&pkg)).expect("the pie reopens");
        assert_eq!(chart(&pkg).series.len(), 3);
        let mut cd = chart(&pkg);
        cd.kind = "column".into();
        cd.edited = true;
        pkg.workbook.sheets[0].drawings[0].kind = DrawingKind::Chart(cd);
        let mut pkg = load_xlsx(&save_xlsx(&pkg)).expect("the column chart reopens");
        let col = chart(&pkg);
        assert_eq!(col.kind, "column");
        assert_eq!(col.series.len(), 3, "converting back keeps all three");
        assert_eq!(col.series[2].name, "North");
        assert_eq!(col.series[2].values, vec![5.0, 6.0]);

        // And back to pie again: three trips through the writer, still three.
        let mut cd = col;
        cd.kind = "pie".into();
        cd.edited = true;
        pkg.workbook.sheets[0].drawings[0].kind = DrawingKind::Chart(cd);
        let again = chart(&load_xlsx(&save_xlsx(&pkg)).expect("the pie reopens again"));
        assert_eq!(again.kind, "pie");
        assert_eq!(again.series.len(), 3);
        let names: Vec<&str> = again.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["East", "West", "North"]);
        assert_eq!(
            again.series[1].values_ref.as_ref().map(|r| r.range),
            Some((1, 2, 2, 2))
        );
    }

    /// The common case, through the same file path: one series in, one series
    /// out, and the part is unchanged when it is written again. Whatever the
    /// multi-series arm changed, it did not move the pie everybody has.
    #[test]
    fn a_one_series_pie_reopens_unchanged() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource, DrawingKind};
        let src = |range| ChartSource {
            sheet: "Sheet1".into(),
            range,
            cat_col: 0,
        };
        let chart = |p: &SheetPackage| match &p.workbook.sheets[0]
            .drawings
            .first()
            .expect("chart drawing")
            .kind
        {
            DrawingKind::Chart(c) => c.clone(),
            other => panic!("expected a chart drawing, got {other:?}"),
        };
        let part_of = |p: &SheetPackage| {
            String::from_utf8(p.part("xl/charts/chart1.xml").expect("chart part").to_vec())
                .expect("utf-8")
        };
        let mut pkg = new_xlsx();
        pkg.add_chart(
            0,
            (5, 0),
            (20, 8),
            &ChartData {
                title: "Sales".into(),
                kind: "pie".into(),
                categories: vec!["Q1".into(), "Q2".into()],
                series: vec![ChartSeries {
                    name: "East".into(),
                    values: vec![1.0, 2.0],
                    col: Some(1),
                    values_ref: Some(src((1, 1, 2, 1))),
                    ..Default::default()
                }],
                source: Some(src((0, 0, 2, 1))),
                categories_ref: Some(src((1, 0, 2, 0))),
                ..Default::default()
            },
        );
        let re = load_xlsx(&save_xlsx(&pkg)).expect("reopen");
        let back = chart(&re);
        assert_eq!(back.kind, "pie");
        assert_eq!(back.series.len(), 1);
        assert_eq!(back.series[0].name, "East");
        assert_eq!(back.series[0].values, vec![1.0, 2.0]);
        assert!(!back.complex);
        // Edited and saved again, the part is unchanged — one `<c:ser>`, no
        // stray empty slice group from the loop that now writes several.
        let before = part_of(&re);
        let mut edited = re;
        let mut cd = back;
        cd.edited = true;
        edited.workbook.sheets[0].drawings[0].kind = DrawingKind::Chart(cd);
        let re2 = load_xlsx(&save_xlsx(&edited)).expect("reopen after the edit");
        assert_eq!(part_of(&re2), before, "the one-series part did not move");
        assert_eq!(part_of(&re2).matches("<c:ser>").count(), 1);
    }

    #[test]
    fn parse_frozen_pane() {
        let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        let frozen = format!(
            "<worksheet xmlns=\"{ns}\"><sheetViews><sheetView>\
             <pane xSplit=\"2\" ySplit=\"3\" topLeftCell=\"C4\" state=\"frozen\"/>\
             </sheetView></sheetViews><sheetData/></worksheet>"
        );
        assert_eq!(
            parse_worksheet(&frozen, &[], &Default::default()).freeze,
            (3, 2)
        ); // (rows, cols)

        // A plain split pane (scrollbar split, not frozen) is ignored.
        let split = format!(
            "<worksheet xmlns=\"{ns}\"><sheetViews><sheetView>\
             <pane xSplit=\"1\" ySplit=\"1\" state=\"split\"/>\
             </sheetView></sheetViews><sheetData/></worksheet>"
        );
        assert_eq!(
            parse_worksheet(&split, &[], &Default::default()).freeze,
            (0, 0)
        );
    }

    #[test]
    fn parse_hyperlinks() {
        let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        let rns = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let xml = format!(
            "<worksheet xmlns=\"{ns}\" xmlns:r=\"{rns}\"><sheetData/><hyperlinks>\
             <hyperlink ref=\"A1\" r:id=\"rId1\"/>\
             <hyperlink ref=\"B2\" location=\"Sheet2!C3\"/></hyperlinks></worksheet>"
        );
        let mut targets = std::collections::HashMap::new();
        targets.insert("rId1".to_string(), "https://example.com".to_string());
        let sheet = parse_worksheet(&xml, &[], &targets);
        assert_eq!(
            sheet.hyperlinks.get(&(0, 0)).map(String::as_str),
            Some("https://example.com")
        );
        assert_eq!(
            sheet.hyperlinks.get(&(1, 1)).map(String::as_str),
            Some("#Sheet2!C3")
        );
    }

    /// #671 R14: removing a link survives the save, a range link touched
    /// anywhere goes whole, and the others stay byte for byte.
    #[test]
    fn removed_hyperlinks_are_struck_on_save() {
        use crate::edit::{ClearWhat, apply_clear};
        let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        let rns = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let xml = format!(
            "<worksheet xmlns=\"{ns}\" xmlns:r=\"{rns}\"><sheetData/><hyperlinks>\
             <hyperlink ref=\"A1:B2\" r:id=\"rId1\"/>\
             <hyperlink ref=\"D4\" location=\"Sheet2!C3\"/></hyperlinks></worksheet>"
        );
        let mut targets = std::collections::HashMap::new();
        targets.insert("rId1".to_string(), "https://example.com".to_string());
        let load = |xml: &str| parse_worksheet(xml, &[], &targets);
        let book = |sheet: Sheet| crate::sheet::Workbook {
            sheets: vec![sheet],
            ..Default::default()
        };
        // A partial clear: B2 of A1:B2 takes the whole link.
        let mut wb = book(load(&xml));
        assert_eq!(wb.sheets[0].hyperlinks.len(), 5);
        apply_clear(&mut wb, 0, &[(1, 1, 1, 1)], ClearWhat::Hyperlinks, &[]).unwrap();
        assert_eq!(wb.sheets[0].hyperlinks.len(), 1, "A1:B2 went whole");
        let saved = splice_worksheet(&xml, &wb.sheets[0], "<sheetData/>", &[], &mut |_, _| None);
        assert!(!saved.contains("A1:B2"), "{saved}");
        assert!(saved.contains("<hyperlink ref=\"D4\" location=\"Sheet2!C3\"/>"));
        let back = load(&saved);
        assert!(!back.hyperlinks.contains_key(&(1, 1)) && !back.hyperlinks.contains_key(&(0, 0)));
        assert_eq!(back.hyperlinks.len(), 1);
        // A full clear drops the block.
        let mut wb = book(load(&xml));
        apply_clear(
            &mut wb,
            0,
            &[(0, 0, 9, 9)],
            ClearWhat::RemoveHyperlinks,
            &[],
        )
        .unwrap();
        let saved = splice_worksheet(&xml, &wb.sheets[0], "<sheetData/>", &[], &mut |_, _| None);
        assert!(!saved.contains("hyperlink"), "{saved}");
        assert!(load(&saved).hyperlinks.is_empty());
        // Nothing removed: the part is left alone.
        let wb = book(load(&xml));
        assert!(
            splice_worksheet(&xml, &wb.sheets[0], "<sheetData/>", &[], &mut |_, _| None)
                .contains("A1:B2")
        );
    }

    /// #707 r6 M2: a column of 50,000 per-cell links clears and saves in
    /// linear time, and the save keeps only the links left.
    #[test]
    fn clearing_50k_links_and_saving_is_fast() {
        use crate::edit::{ClearWhat, apply_clear};
        let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        let mut links = String::new();
        for r in 1..=50_000 {
            links.push_str(&format!("<hyperlink ref=\"A{r}\" location=\"Sheet2!A1\"/>"));
        }
        let xml = format!(
            "<worksheet xmlns=\"{ns}\"><sheetData/><hyperlinks>{links}</hyperlinks></worksheet>"
        );
        let mut wb = crate::sheet::Workbook {
            sheets: vec![parse_worksheet(&xml, &[], &Default::default())],
            ..Default::default()
        };
        assert_eq!(wb.sheets[0].hyperlinks.len(), 50_000);
        let t = std::time::Instant::now();
        // All but the last link.
        apply_clear(&mut wb, 0, &[(0, 0, 49_998, 0)], ClearWhat::Hyperlinks, &[]).unwrap();
        let saved = splice_worksheet(&xml, &wb.sheets[0], "<sheetData/>", &[], &mut |_, _| None);
        assert!(t.elapsed() < crate::edit::PERF_BOUND, "{:?}", t.elapsed());
        let back = parse_worksheet(&saved, &[], &Default::default());
        assert_eq!(back.hyperlinks.len(), 1);
        assert!(back.hyperlinks.contains_key(&(49_999, 0)));
    }

    #[test]
    fn parse_data_validations() {
        let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        let xml = format!(
            "<worksheet xmlns=\"{ns}\"><sheetData/><dataValidations count=\"2\">\
             <dataValidation type=\"list\" sqref=\"A1:A10\" prompt=\"Pick one\">\
             <formula1>\"Yes,No,Maybe\"</formula1></dataValidation>\
             <dataValidation type=\"whole\" operator=\"between\" sqref=\"B1 B2\">\
             <formula1>1</formula1><formula2>10</formula2></dataValidation>\
             </dataValidations></worksheet>"
        );
        let sheet = parse_worksheet(&xml, &[], &std::collections::HashMap::new());
        assert_eq!(sheet.validations.len(), 2);
        let list = &sheet.validations[0];
        assert!(list.covers(0, 0) && list.covers(9, 0) && !list.covers(10, 0));
        assert_eq!(list.prompt.as_deref(), Some("Pick one"));
        assert_eq!(
            list.list_values(),
            Some(vec!["Yes".into(), "No".into(), "Maybe".into()])
        );
        assert_eq!(list.describe(), "List: Yes, No, Maybe");
        let whole = &sheet.validations[1];
        assert!(whole.covers(0, 1) && whole.covers(1, 1) && !whole.covers(2, 1));
        assert_eq!(whole.describe(), "Whole number between 1 and 10");
    }

    /// Build a small real .xlsx in memory for load tests.
    fn fixture() -> Vec<u8> {
        let sheet1 = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:C3"/><cols><col min="2" max="2" width="20" customWidth="1"/></cols><sheetData><row r="1" ht="30" customHeight="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>42</v></c><c r="C1" s="1"><v>45306</v></c></row><row r="2"><c r="A2" t="b"><v>1</v></c><c r="B2"><f>B1*2</f><v>84</v></c><c r="C2" t="inlineStr"><is><t>inline!</t></is></c></row><row r="3"><c r="B3"><f t="shared" ref="B3:B4" si="0">B2+1</f><v>85</v></c></row><row r="4"><c r="B4"><f t="shared" si="0"/><v>86</v></c></row></sheetData><mergeCells count="1"><mergeCell ref="A5:B6"/></mergeCells><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/></worksheet>"#;
        let sst = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1"><si><t>hello</t></si></sst>"#;
        let styles = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="2"><font><sz val="11"/></font><font><b/><color rgb="FFFF0000"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf/></cellStyleXfs><cellXfs count="2"><xf numFmtId="0" fontId="0"/><xf numFmtId="14" fontId="1" applyNumberFormat="1"/></cellXfs></styleSheet>"#;
        let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets><definedNames><definedName name="Total">Data!$B$2</definedName><definedName name="_xlnm.Print_Area" localSheetId="0">Data!$A$1:$C$3</definedName></definedNames><calcPr calcId="191029"/></workbook>"#;
        let wb_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain" Target="calcChain.xml"/></Relationships>"#;
        let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/calcChain.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.calcChain+xml"/></Types>"#;
        let calc_chain = r#"<?xml version="1.0"?><calcChain xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><c r="B2" i="1"/></calcChain>"#;

        write_zip(&[
            ("[Content_Types].xml".into(), content_types.into()),
            ("_rels/.rels".into(), root_rels.into()),
            ("xl/workbook.xml".into(), workbook.into()),
            ("xl/_rels/workbook.xml.rels".into(), wb_rels.into()),
            ("xl/worksheets/sheet1.xml".into(), sheet1.into()),
            ("xl/styles.xml".into(), styles.into()),
            ("xl/sharedStrings.xml".into(), sst.into()),
            ("xl/calcChain.xml".into(), calc_chain.into()),
        ])
    }

    /// The #604 workbook as openpyxl writes it on Windows: two sheets, the
    /// second active, and a CR LF written literally inside a `<t>`.
    fn active_second_sheet_xlsx() -> Vec<u8> {
        let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let sheet1 = format!(
            r#"<worksheet xmlns="{ns}"><sheetViews><sheetView workbookViewId="0"><selection activeCell="A1" sqref="A1"/></sheetView></sheetViews><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>wrong sheet</t></is></c></row></sheetData></worksheet>"#
        );
        let sheet2 = format!(
            "<worksheet xmlns=\"{ns}\"><sheetViews><sheetView tabSelected=\"1\" workbookViewId=\"0\"/></sheetViews><sheetData>\
             <row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>Name</t></is></c><c r=\"B1\" t=\"inlineStr\"><is><t>Note</t></is></c></row>\
             <row r=\"2\"><c r=\"A2\" t=\"inlineStr\"><is><t>Z\u{fc}rich</t></is></c><c r=\"B2\" t=\"inlineStr\"><is><t>line1\r\nline2</t></is></c></row>\
             </sheetData></worksheet>"
        );
        let workbook = format!(
            r#"<workbook xmlns="{ns}" xmlns:r="{rel}"><workbookPr/><bookViews><workbookView visibility="visible" minimized="0" showHorizontalScroll="1" showVerticalScroll="1" showSheetTabs="1" tabRatio="600" firstSheet="0" activeTab="1" autoFilterDateGrouping="1"/></bookViews><sheets><sheet name="First" sheetId="1" r:id="rId1"/><sheet name="Data" sheetId="2" r:id="rId2"/></sheets></workbook>"#
        );
        let wb_rels = format!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{rel}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="{rel}/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#
        );
        let root_rels = format!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{rel}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#
        );
        let content_types = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;
        write_zip(&[
            ("[Content_Types].xml".into(), content_types.into()),
            ("_rels/.rels".into(), root_rels.into_bytes()),
            ("xl/workbook.xml".into(), workbook.into_bytes()),
            ("xl/_rels/workbook.xml.rels".into(), wb_rels.into_bytes()),
            ("xl/worksheets/sheet1.xml".into(), sheet1.into_bytes()),
            ("xl/worksheets/sheet2.xml".into(), sheet2.into_bytes()),
        ])
    }

    fn part_text(pkg: &SheetPackage, name: &str) -> String {
        String::from_utf8_lossy(pkg.part(name).unwrap()).into_owned()
    }

    #[test]
    fn the_active_tab_loads_and_its_csv_is_excels() {
        let pkg = load_xlsx(&active_second_sheet_xlsx()).unwrap();
        let wb = &pkg.workbook;
        assert_eq!(wb.active_tab, 1);
        let csv = crate::sheet::sheet_to_csv(&wb.sheets[wb.active_tab], &wb.styles, false);
        assert_eq!(csv, "Name,Note\r\nZ\u{fc}rich,\"line1\nline2\"\r\n");
    }

    #[test]
    fn saving_moves_the_active_tab_and_its_selection_together() {
        let mut pkg = load_xlsx(&active_second_sheet_xlsx()).unwrap();
        pkg.workbook.active_tab = 0;
        let pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(pkg.workbook.active_tab, 0);
        assert!(part_text(&pkg, "xl/workbook.xml").contains("activeTab=\"0\""));
        // Only the active sheet is selected: two would open grouped.
        assert!(part_text(&pkg, "xl/worksheets/sheet1.xml").contains("tabSelected=\"1\""));
        assert!(!part_text(&pkg, "xl/worksheets/sheet2.xml").contains("tabSelected"));
        // An unchanged active tab leaves workbook.xml as it was.
        let again = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(
            part_text(&again, "xl/workbook.xml"),
            part_text(&pkg, "xl/workbook.xml")
        );
    }

    /// The rewrite follows a namespace prefix (`x:workbookView`), and an
    /// attribute however it is spelled, so none is added twice.
    #[test]
    fn the_active_tab_rewrite_handles_prefixes_and_attribute_spelling() {
        let wb = r#"<x:workbook xmlns:x="ns"><x:bookViews><x:workbookView activeTab = '1'/></x:bookViews><x:sheets/></x:workbook>"#;
        let out = set_active_tab(wb, 2);
        assert_eq!(out.matches("activeTab").count(), 1, "{out}");
        assert!(out.contains(r#"<x:workbookView activeTab="2"/>"#), "{out}");
        assert_eq!(parse_active_tab(&out), 2);
        let bare = r#"<x:workbook xmlns:x="ns"><x:sheets/></x:workbook>"#;
        let out = set_active_tab(bare, 1);
        assert!(
            out.contains(
                r#"<x:bookViews><x:workbookView activeTab="1"/></x:bookViews><x:sheets/>"#
            ),
            "{out}"
        );
        let ws = "<x:worksheet><x:sheetViews><x:sheetView\ttabSelected=\"1\" workbookViewId=\"0\"/></x:sheetViews></x:worksheet>";
        let out = set_tab_selected(ws, false);
        assert!(!out.contains("tabSelected"), "{out}");
        let out = set_tab_selected(&out, true);
        assert_eq!(out.matches("tabSelected").count(), 1, "{out}");
        // A lookalike name inside another attribute is not the attribute.
        assert_eq!(
            tag_attr(r#"<a xtabSelected="0" tabSelected="1">"#, "tabSelected"),
            Some("1")
        );
    }

    /// A file that marks its tab `tabSelected='1'` (single quotes) still has
    /// the mark moved with the active tab.
    #[test]
    fn a_single_quoted_tab_selection_moves_with_the_active_tab() {
        let mut pkg = load_xlsx(&active_second_sheet_xlsx()).unwrap();
        let sheet2 = part_text(&pkg, "xl/worksheets/sheet2.xml")
            .replace("tabSelected=\"1\"", "tabSelected='1'");
        pkg.set_part("xl/worksheets/sheet2.xml", sheet2.into_bytes());
        pkg.workbook.active_tab = 0;
        let saved = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert!(part_text(&saved, "xl/worksheets/sheet1.xml").contains("tabSelected=\"1\""));
        assert!(!part_text(&saved, "xl/worksheets/sheet2.xml").contains("tabSelected"));
    }

    /// #603: the model's date system is written back. An imported 1904
    /// workbook starts from new_xlsx's 1900 part and must not stay 1900.
    #[test]
    fn save_writes_the_models_date1904() {
        let mut pkg = new_xlsx();
        pkg.workbook.date1904 = true;
        let back = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert!(back.workbook.date1904);
        let mut pkg = back;
        pkg.workbook.date1904 = false;
        assert!(!load_xlsx(&save_xlsx(&pkg)).unwrap().workbook.date1904);
    }

    #[test]
    fn set_date1904_patches_inserts_and_removes() {
        let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        // No workbookPr: one goes before bookViews/sheets.
        let bare = format!("<workbook xmlns=\"{ns}\"><sheets/></workbook>");
        assert_eq!(
            set_date1904(&bare, true),
            format!("<workbook xmlns=\"{ns}\"><workbookPr date1904=\"1\"/><sheets/></workbook>")
        );
        assert_eq!(set_date1904(&bare, false), bare);
        // A prefixed one keeps its other attributes.
        let x = format!(
            "<x:workbook xmlns:x=\"{ns}\"><x:workbookPr defaultThemeVersion=\"1\"/><x:sheets/></x:workbook>"
        );
        let on = set_date1904(&x, true);
        assert!(
            on.contains("<x:workbookPr date1904=\"1\" defaultThemeVersion=\"1\"/>"),
            "{on}"
        );
        assert_eq!(set_date1904(&on, true), on);
        assert_eq!(set_date1904(&on, false), x);
        // A false spelling already agrees with 1900.
        let f = format!("<workbook xmlns=\"{ns}\"><workbookPr date1904=\"false\"/></workbook>");
        assert_eq!(set_date1904(&f, false), f);
    }

    /// Excel's *Always create backup* (`<workbookPr backupFile>`): the save
    /// path honours it (xlsxy), so it is read from the package, not the
    /// model. Only "1"/"true" turn it on; anything else (or no attribute,
    /// no element, no part) means off.
    #[test]
    fn always_create_backup_reads_workbook_pr() {
        let pkg_with = |wb_pr: &str| {
            let mut pkg = new_xlsx();
            pkg.set_part(
                "xl/workbook.xml",
                format!("<workbook>{wb_pr}</workbook>").into_bytes(),
            );
            pkg
        };
        assert!(pkg_with(r#"<workbookPr backupFile="1"/>"#).always_create_backup());
        assert!(pkg_with(r#"<workbookPr backupFile="true"/>"#).always_create_backup());
        assert!(!pkg_with(r#"<workbookPr backupFile="0"/>"#).always_create_backup());
        assert!(!pkg_with(r#"<workbookPr backupFile="false"/>"#).always_create_backup());
        assert!(!pkg_with("<workbookPr/>").always_create_backup());
        assert!(!pkg_with("").always_create_backup());
        assert!(!new_xlsx().always_create_backup());
    }

    /// The corpus spells the boolean the LibreOffice way
    /// (`backupFile="false"`, corpus/xlsx/calc-3d.xlsx); flipping just the
    /// attribute turns the flag on.
    #[test]
    fn always_create_backup_reads_the_corpus_attribute() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../corpus/xlsx/calc-3d.xlsx");
        let bytes = std::fs::read(path).expect("corpus/xlsx/calc-3d.xlsx exists");
        let mut pkg = load_xlsx(&bytes).expect("corpus loads");
        let wb = pkg
            .part("xl/workbook.xml")
            .expect("workbook part is xl/workbook.xml");
        assert!(!pkg.always_create_backup());
        let xml = String::from_utf8_lossy(wb).replace("backupFile=\"false\"", "backupFile=\"1\"");
        pkg.set_part("xl/workbook.xml", xml.into_bytes());
        assert!(pkg.always_create_backup());
    }

    #[test]
    fn a_workbook_without_book_views_gains_one_for_a_later_active_tab() {
        let mut pkg = new_xlsx();
        // The first sheet starts selected, as Excel saves it.
        let sheet1 = part_text(&pkg, "xl/worksheets/sheet1.xml")
            .replace("<sheetView ", "<sheetView tabSelected=\"1\" ");
        assert!(sheet1.contains("tabSelected=\"1\""));
        pkg.set_part("xl/worksheets/sheet1.xml", sheet1.into_bytes());
        pkg.add_sheet("Two");
        pkg.workbook.active_tab = 1;
        let saved = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(saved.workbook.active_tab, 1);
        let xml = part_text(&saved, "xl/workbook.xml");
        let views = xml.find("<bookViews><workbookView activeTab=\"1\"/></bookViews>");
        assert!(
            views.is_some_and(|v| v < xml.find("<sheets").unwrap()),
            "{xml}"
        );
        // The first sheet is no longer selected.
        assert!(!part_text(&saved, "xl/worksheets/sheet1.xml").contains("tabSelected=\"1\""));
        // Removing a sheet before the active one keeps the same sheet active.
        let mut three = saved;
        three.add_sheet("Three");
        three.workbook.active_tab = 2;
        assert!(three.remove_sheet(0));
        assert_eq!(three.workbook.active_tab, 1);
        assert!(three.remove_sheet(1));
        assert_eq!(three.workbook.active_tab, 0);
    }

    #[test]
    fn hostile_worksheet_loads_and_saves_without_panic() {
        // A crafted worksheet: r="0" (would underflow), a non-ASCII 8-byte
        // rgb (would slice on a char boundary), and a comment containing a
        // "<sheetData>" literal ahead of the real element (would misdirect
        // the splice). None may panic or corrupt the save.
        let sheet1 = concat!(
            r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">"#,
            "<!-- <sheetData><row r=\"1\"><c r=\"A1\"/></row></sheetData> -->",
            r#"<dimension ref="A1"/><sheetData><row r="0"><c r="A1"><v>7</v></c></row>"#,
            r#"<row r="2"><c r="A2"><f>A1+1</f><v>8</v></c></row></sheetData></worksheet>"#,
        );
        let styles = concat!(
            r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">"#,
            r#"<fonts count="1"><font><color rgb="aébcdef"/></font></fonts>"#,
            r#"<fills count="1"><fill><patternFill patternType="none"/></fill></fills>"#,
            r#"<borders count="1"><border/></borders><cellStyleXfs count="1"><xf/></cellStyleXfs>"#,
            r#"<cellXfs count="1"><xf numFmtId="0" fontId="0"/></cellXfs></styleSheet>"#,
        );
        let workbook = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let wb_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
        let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;
        let data = write_zip(&[
            ("[Content_Types].xml".into(), content_types.into()),
            ("_rels/.rels".into(), root_rels.into()),
            ("xl/workbook.xml".into(), workbook.into()),
            ("xl/_rels/workbook.xml.rels".into(), wb_rels.into()),
            ("xl/worksheets/sheet1.xml".into(), sheet1.into()),
            ("xl/styles.xml".into(), styles.into()),
        ]);
        let mut pkg = load_xlsx(&data).expect("hostile file still loads");
        // r="0" clamped to row 0 (A1); the formula recalculates.
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        // Save must not corrupt: the comment stays a comment, real data spliced.
        let out = save_xlsx(&pkg);
        let reopened = load_xlsx(&out).expect("re-save reloads");
        let ws = String::from_utf8_lossy(reopened.part("xl/worksheets/sheet1.xml").unwrap());
        assert!(ws.contains("<!-- "), "comment preserved");
        assert!(ws.contains("<c r=\"A2\""), "real cell present");
    }

    #[test]
    fn load_reads_values_types_and_styles() {
        let pkg = load_xlsx(&fixture()).expect("load");
        let wb = &pkg.workbook;
        assert_eq!(wb.sheets.len(), 1);
        let s = &wb.sheets[0];
        assert_eq!(s.name, "Data");
        assert_eq!(s.cell(0, 0).unwrap().value, CellValue::Text("hello".into()));
        assert_eq!(s.cell(0, 1).unwrap().value, CellValue::Number(42.0));
        assert_eq!(s.cell(1, 0).unwrap().value, CellValue::Bool(true));
        assert_eq!(
            s.cell(1, 2).unwrap().value,
            CellValue::Text("inline!".into())
        );
        // Formula with cached value.
        let b2 = s.cell(1, 1).unwrap();
        assert_eq!(b2.formula.as_deref(), Some("B1*2"));
        assert_eq!(b2.value, CellValue::Number(84.0));
        // Shared formula expanded on the follower.
        let b4 = s.cell(3, 1).unwrap();
        assert_eq!(b4.formula.as_deref(), Some("B3+1"));
        assert_eq!(b4.value, CellValue::Number(86.0));
        // Styles: xf 1 is a bold red date.
        let xf = wb.styles.xf(s.cell(0, 2).unwrap().style);
        assert_eq!(xf.numfmt, NumFmt::Date);
        assert!(xf.bold);
        assert_eq!(xf.color, Some((255, 0, 0)));
        // Defined names: real ones and the built-in print area load (the
        // print area has to follow structural edits), keeping its scope.
        assert_eq!(wb.defined_names.len(), 2);
        assert_eq!(wb.defined_name("total", 0), Some("Data!$B$2"));
        assert_eq!(
            wb.defined_names[1],
            DefinedName {
                name: "_xlnm.Print_Area".into(),
                scope: Some(0),
                formula: "Data!$A$1:$C$3".into(),
            }
        );
        // Column width + row attrs + merges.
        assert_eq!(s.col_width(1), 20.0);
        assert!(s.row_attrs.get(&0).unwrap().contains("customHeight"));
        assert_eq!(s.merges, vec![(4, 0, 5, 1)]);
    }

    #[test]
    fn calc_chain_first_relationship_keeps_rels_root() {
        let mut pkg = load_xlsx(&fixture()).expect("load");
        let rels = part_text(&pkg, "xl/_rels/workbook.xml.rels");
        let calc = "<Relationship Id=\"rId4\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain\" Target=\"calcChain.xml\"/>";
        let first =
            rels.replace(calc, "")
                .replacen("<Relationship ", &format!("{calc}<Relationship "), 1);
        assert!(first.contains(&format!("relationships\">{calc}")));
        pkg.set_part("xl/_rels/workbook.xml.rels", first.into_bytes());

        let saved = load_xlsx(&save_xlsx(&pkg)).expect("reload");
        let rels = part_text(&saved, "xl/_rels/workbook.xml.rels");
        assert!(rels.contains("<Relationships xmlns="), "{rels}");
        assert!(!rels.contains("calcChain"), "{rels}");
        for target in ["worksheets/sheet1.xml", "styles.xml", "sharedStrings.xml"] {
            assert!(rels.contains(&format!("Target=\"{target}\"")), "{rels}");
        }
    }

    #[test]
    fn remove_element_containing_skips_longer_name() {
        let rels = "<Relationships xmlns=\"r\"><Relationship Id=\"rId1\" Target=\"a.xml\"/><Relationship Id=\"rId2\" Target=\"b.xml\"/></Relationships>";
        assert_eq!(
            remove_element_containing(rels, "<Relationship", "a.xml"),
            "<Relationships xmlns=\"r\"><Relationship Id=\"rId2\" Target=\"b.xml\"/></Relationships>"
        );
        let caches = "<pivotCaches><pivotCache cacheId=\"1\" r:id=\"rId5\"/></pivotCaches>";
        assert_eq!(
            remove_element_containing(caches, "<pivotCache", "rId5"),
            "<pivotCaches></pivotCaches>"
        );
        // A prefix that already ends the name (`<sheet `) is taken as given.
        let sheets = "<sheets><sheet name=\"A\" r:id=\"rId1\"/></sheets>";
        assert_eq!(
            remove_element_containing(sheets, "<sheet ", "rId1"),
            "<sheets></sheets>"
        );
    }

    #[test]
    fn removing_a_first_child_keeps_each_wrapper() {
        // The pivot cache is the only child of <pivotCaches>, and the pivot
        // table the first relationship of its sheet's rels.
        let mut pkg = load_xlsx(&pivot_fixture()).unwrap();
        assert!(pkg.remove_pivot(0));
        let wb = part_text(&pkg, "xl/workbook.xml");
        assert!(!wb.contains("pivotCache"), "{wb}");
        assert!(wb.contains("</sheets></workbook>"), "{wb}");
        let ws_rels = part_text(&pkg, "xl/worksheets/_rels/sheet2.xml.rels");
        assert!(ws_rels.contains("<Relationships xmlns="), "{ws_rels}");
        assert!(!ws_rels.contains("pivotTable"), "{ws_rels}");
        let wb_rels = part_text(&pkg, "xl/_rels/workbook.xml.rels");
        assert!(!wb_rels.contains("pivotCache"), "{wb_rels}");
        assert!(load_xlsx(&save_xlsx(&pkg)).is_ok());

        // The sheet-removal path: sheet 1's relationship is rId1, the first.
        let mut pkg = load_xlsx(&pivot_fixture()).unwrap();
        assert!(pkg.remove_sheet(0));
        let wb_rels = part_text(&pkg, "xl/_rels/workbook.xml.rels");
        assert!(wb_rels.contains("<Relationships xmlns="), "{wb_rels}");
        assert!(!wb_rels.contains("worksheets/sheet1.xml"), "{wb_rels}");
        assert!(wb_rels.contains("worksheets/sheet2.xml"), "{wb_rels}");
        let saved = load_xlsx(&save_xlsx(&pkg)).expect("reload");
        assert_eq!(saved.workbook.sheets.len(), 1);
        assert_eq!(saved.workbook.sheets[0].name, "Report");
    }

    #[test]
    fn save_round_trips_and_drops_calc_chain() {
        let pkg = load_xlsx(&fixture()).expect("load");
        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).expect("reload");
        let (s1, s2) = (&pkg.workbook.sheets[0], &pkg2.workbook.sheets[0]);
        assert_eq!(s1.cells, s2.cells);
        assert_eq!(s1.merges, s2.merges);
        assert_eq!(s1.row_attrs, s2.row_attrs);
        // calcChain gone, everywhere.
        assert!(pkg2.part("xl/calcChain.xml").is_none());
        let ct = String::from_utf8_lossy(pkg2.part("[Content_Types].xml").unwrap()).into_owned();
        assert!(!ct.contains("calcChain"));
        let rels =
            String::from_utf8_lossy(pkg2.part("xl/_rels/workbook.xml.rels").unwrap()).into_owned();
        assert!(!rels.contains("calcChain"));
        // fullCalcOnLoad set.
        let wb = String::from_utf8_lossy(pkg2.part("xl/workbook.xml").unwrap()).into_owned();
        assert!(wb.contains("fullCalcOnLoad=\"1\""));
        // Unmodeled sheet furniture preserved.
        let ws =
            String::from_utf8_lossy(pkg2.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
        assert!(ws.contains("<pageMargins"));
        assert!(ws.contains("<mergeCells"));
    }

    #[test]
    fn edits_survive_a_save() {
        let mut pkg = load_xlsx(&fixture()).expect("load");
        // New text (goes to shared strings), new number, edited formula.
        pkg.workbook.sheets[0].set_cell(9, 0, Cell::text("fresh text"));
        pkg.workbook.sheets[0].set_cell(9, 1, Cell::number(2.5));
        pkg.workbook.sheets[0].set_cell(
            9,
            2,
            Cell {
                value: CellValue::Number(126.0),
                formula: Some("B2+B1".to_string()),
                ..Cell::default()
            },
        );
        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).expect("reload");
        let s = &pkg2.workbook.sheets[0];
        assert_eq!(
            s.cell(9, 0).unwrap().value,
            CellValue::Text("fresh text".into())
        );
        assert_eq!(s.cell(9, 1).unwrap().value, CellValue::Number(2.5));
        assert_eq!(s.cell(9, 2).unwrap().formula.as_deref(), Some("B2+B1"));
        // Existing "hello" is still shared-string index 0 (table appended).
        let sst = String::from_utf8_lossy(pkg2.part("xl/sharedStrings.xml").unwrap()).into_owned();
        assert!(sst.find("hello").unwrap() < sst.find("fresh text").unwrap());
        // The fixture's inline string joins the table on save (Excel accepts
        // either form), so hello + inline! + fresh text = 3.
        assert!(sst.contains("uniqueCount=\"3\""));
    }

    #[test]
    fn new_workbook_round_trips() {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::text("title"));
        pkg.workbook.sheets[0].set_cell(1, 0, Cell::number(3.25));
        pkg.workbook.sheets[0].set_cell(
            2,
            0,
            Cell {
                value: CellValue::Number(6.5),
                formula: Some("A2*2".to_string()),
                ..Cell::default()
            },
        );
        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).expect("reload new workbook");
        let s = &pkg2.workbook.sheets[0];
        assert_eq!(s.name, "Sheet1");
        assert_eq!(s.cell(0, 0).unwrap().value, CellValue::Text("title".into()));
        assert_eq!(s.cell(1, 0).unwrap().value, CellValue::Number(3.25));
        assert_eq!(s.cell(2, 0).unwrap().formula.as_deref(), Some("A2*2"));
    }

    #[test]
    fn authored_styles_round_trip() {
        use crate::sheet::{Align, Xf};
        let mut pkg = new_xlsx();
        // Bold red, right-aligned, with a custom number format and a fill.
        let idx = pkg.workbook.styles.intern(Xf {
            bold: true,
            italic: true,
            color: Some((255, 0, 0)),
            fill: Some((255, 255, 0)),
            align: Align::Right,
            code: Some("0.00%".to_string()),
            numfmt: crate::sheet::NumFmt::Percent { decimals: 2 },
            ..Xf::default()
        });
        pkg.workbook.sheets[0].set_cell(
            0,
            0,
            Cell {
                value: CellValue::Number(0.5),
                style: idx,
                ..Cell::default()
            },
        );
        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).expect("reload authored styles");
        let s = &pkg2.workbook.sheets[0];
        let cell = s.cell(0, 0).unwrap();
        let xf = pkg2.workbook.styles.xf(cell.style);
        assert!(xf.bold, "bold survived");
        assert!(xf.italic, "italic survived");
        assert_eq!(xf.color, Some((255, 0, 0)));
        assert_eq!(xf.fill, Some((255, 255, 0)));
        assert_eq!(xf.align, Align::Right);
        assert_eq!(xf.code.as_deref(), Some("0.00%"));
        // The original default xf is untouched (no bold/fill/align bleed).
        let d = pkg2.workbook.styles.xf(0);
        assert!(!d.bold && !d.italic && d.fill.is_none() && d.align == Align::General);
    }

    /// The Task-3 contract: a `cell.format`-style patch (built via
    /// [`crate::format::FormatPatch`]/[`crate::format::apply_patch_to_xf`],
    /// exactly as xlsxy's `control.rs` uses them) survives `save_xlsx` →
    /// `load_xlsx` intact — not just an `Xf` authored directly.
    #[test]
    fn format_patch_style_round_trips_through_save_and_load() {
        use crate::format::{FormatPatch, apply_patch_to_xf};
        use crate::sheet::Align;

        let pairs = vec![
            ("bold".to_string(), "true".to_string()),
            ("fillColor".to_string(), "#ABCDEF".to_string()),
            ("align".to_string(), "center".to_string()),
            ("numFmt".to_string(), "0.00".to_string()),
        ];
        let patch = FormatPatch::parse(&pairs).unwrap();

        let mut pkg = new_xlsx();
        let base = pkg.workbook.styles.xf(0);
        let xf = apply_patch_to_xf(&base, &patch);
        let idx = pkg.workbook.styles.intern(xf);
        pkg.workbook.sheets[0].set_cell(
            0,
            0,
            Cell {
                value: CellValue::Number(1.5),
                style: idx,
                ..Cell::default()
            },
        );

        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).expect("reload format-patch styles");
        let cell = pkg2.workbook.sheets[0].cell(0, 0).unwrap();
        let xf2 = pkg2.workbook.styles.xf(cell.style);
        assert!(xf2.bold);
        assert_eq!(xf2.fill, Some((0xAB, 0xCD, 0xEF)));
        assert_eq!(xf2.align, Align::Center);
        assert_eq!(xf2.code.as_deref(), Some("0.00"));
    }

    #[test]
    fn a_typed_entry_keeps_what_the_model_does_not_carry_of_a_loaded_style() {
        // A General xf with an underlined font, a real (dashed) border, a
        // vertical alignment and protection: none of them in `Xf`.
        let mut pkg = new_xlsx();
        let xml = String::from_utf8(pkg.part("xl/styles.xml").unwrap().to_vec()).unwrap();
        let nf = read_count(&xml, "<fonts");
        let nb = read_count(&xml, "<borders");
        let mut xml = bump_count(&xml, "<fonts", 1);
        xml = xml.replacen(
            "</fonts>",
            "<font><u/><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>",
            1,
        );
        xml = bump_count(&xml, "<borders", 1);
        xml = xml.replacen(
            "</borders>",
            "<border><left style=\"dashed\"/><right/><top/><bottom/><diagonal/></border></borders>",
            1,
        );
        xml = bump_count(&xml, "<cellXfs", 1);
        xml = xml.replacen(
            "</cellXfs>",
            &format!(
                "<xf numFmtId=\"0\" fontId=\"{nf}\" fillId=\"0\" borderId=\"{nb}\" xfId=\"0\" applyFont=\"1\" applyBorder=\"1\"><alignment vertical=\"top\" indent=\"1\"/><protection locked=\"0\"/></xf></cellXfs>"
            ),
            1,
        );
        pkg.set_part("xl/styles.xml", xml.into_bytes());
        let mut pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let src = pkg.workbook.styles.xfs.len() as u32 - 1;
        for (c, text) in [
            "1/15/2024",
            "'x",
            "a
b",
        ]
        .iter()
        .enumerate()
        {
            pkg.workbook.sheets[0].set_cell(
                0,
                c as u32,
                crate::sheet::Cell {
                    style: src,
                    ..crate::sheet::Cell::default()
                },
            );
            let cell =
                crate::entry::entry_cell(&mut pkg.workbook, 0, 0, c as u32, text, None).unwrap();
            assert_ne!(cell.style, src, "{text} derives a new xf");
            pkg.workbook.sheets[0].set_cell(0, c as u32, cell);
        }
        // A format command on the same cell (bold): a new font, the rest kept.
        let mut bold = pkg.workbook.styles.xf(src);
        bold.bold = true;
        let b = pkg.workbook.styles.intern(bold);
        pkg.workbook.sheets[0].set_cell(
            1,
            0,
            crate::sheet::Cell {
                style: b,
                ..crate::sheet::Cell::text("b")
            },
        );

        let saved = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let styles = String::from_utf8(saved.part("xl/styles.xml").unwrap().to_vec()).unwrap();
        let xfs = cell_xf_elements(&styles);
        let style_of = |r: u32, c: u32| saved.workbook.sheets[0].cell(r, c).unwrap().style as usize;
        let attr = |i: usize, a: &str| tag_attr(xfs[i], a).map(str::to_string);
        for c in 0..3 {
            let i = style_of(0, c);
            assert_eq!(attr(i, "fontId"), Some(nf.to_string()), "{}", xfs[i]);
            assert_eq!(attr(i, "borderId"), Some(nb.to_string()), "{}", xfs[i]);
            assert!(xfs[i].contains("vertical=\"top\""), "{}", xfs[i]);
            assert!(xfs[i].contains("indent=\"1\""), "{}", xfs[i]);
            assert!(xfs[i].contains("<protection locked=\"0\"/>"), "{}", xfs[i]);
        }
        let date = style_of(0, 0);
        assert_eq!(
            saved.workbook.styles.xf(date as u32).code.as_deref(),
            Some("m/d/yyyy")
        );
        assert!(xfs[style_of(0, 1)].contains("quotePrefix=\"1\""));
        assert!(xfs[style_of(0, 2)].contains("wrapText=\"1\""));
        // Bold minted its own font but kept the border and the alignment.
        let i = style_of(1, 0);
        assert_ne!(attr(i, "fontId"), Some(nf.to_string()));
        assert_eq!(attr(i, "borderId"), Some(nb.to_string()));
        assert!(saved.workbook.styles.xf(i as u32).bold);
        assert!(xfs[i].contains("vertical=\"top\""));
    }

    #[test]
    fn a_font_edit_reads_fonts_with_comments_in_them() {
        // A commented-out font among the `<fonts>`, and a comment inside the
        // edited one that looks like its end tag.
        let styles = concat!(
            "<styleSheet><fonts count=\"2\"><!-- <font><b/></font> -->",
            "<font><sz val=\"11\"/></font>",
            "<font><!-- </font> --><u/><sz val=\"11\"/><name val=\"Calibri\"/></font>",
            "</fonts></styleSheet>"
        );
        let fonts = font_elements(styles);
        assert_eq!(fonts.len(), 2);
        let names: Vec<String> = child_elements(fonts[1])
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["u", "sz", "name"]);
        let to = Xf {
            bold: true,
            ..Xf::default()
        };
        let out = edit_font(fonts[1], &Xf::default(), &to, |s| format!("{s}"));
        assert_eq!(
            out,
            "<font><b/><u/><sz val=\"11\"/><name val=\"Calibri\"/></font>"
        );
    }

    #[test]
    fn a_font_edit_keeps_what_the_model_does_not_carry_of_a_loaded_font() {
        // An underlined, theme-coloured minor-scheme font with an explicit
        // `<b val="0"/>`, and a `<dxf>` font that is not one of `<fonts>`.
        let mut pkg = new_xlsx();
        let xml = String::from_utf8(pkg.part("xl/styles.xml").unwrap().to_vec()).unwrap();
        let nf = read_count(&xml, "<fonts");
        let mut xml = bump_count(&xml, "<fonts", 1);
        xml = xml.replacen(
            "</fonts>",
            "<font><b val=\"0\"/><u/><sz val=\"11\"/><color theme=\"1\"/><name val=\"Calibri\"/><family val=\"2\"/><scheme val=\"minor\"/></font></fonts>",
            1,
        );
        xml = bump_count(&xml, "<cellXfs", 1);
        xml = xml.replacen(
            "</cellXfs>",
            &format!(
                "<xf numFmtId=\"0\" fontId=\"{nf}\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/></cellXfs>"
            ),
            1,
        );
        xml = xml.replacen(
            "</cellStyles>",
            "</cellStyles><dxfs count=\"1\"><dxf><font><i/><strike/></font></dxf></dxfs>",
            1,
        );
        assert_eq!(font_elements(&xml).len(), nf as usize + 1);
        pkg.set_part("xl/styles.xml", xml.into_bytes());
        let mut pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let src = pkg.workbook.styles.xfs.len() as u32 - 1;
        let base = pkg.workbook.styles.xf(src);
        assert!(!base.bold);
        assert_eq!(base.color, None);

        let mut bold = base.clone();
        bold.bold = true;
        let mut red = base.clone();
        red.color = Some((0xC0, 0x00, 0x00));
        let mut named = base.clone();
        named.font_name = Some("Arial".into());
        for (c, xf) in [bold, red, named].into_iter().enumerate() {
            let style = pkg.workbook.styles.intern(xf);
            pkg.workbook.sheets[0].set_cell(
                0,
                c as u32,
                crate::sheet::Cell {
                    style,
                    ..crate::sheet::Cell::text("x")
                },
            );
        }

        let saved = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let styles = String::from_utf8(saved.part("xl/styles.xml").unwrap().to_vec()).unwrap();
        let xfs = cell_xf_elements(&styles);
        let fonts = font_elements(&styles);
        let font_of = |c: u32| {
            let style = saved.workbook.sheets[0].cell(0, c).unwrap().style;
            let id: usize = tag_attr(xfs[style as usize], "fontId")
                .unwrap()
                .parse()
                .unwrap();
            (saved.workbook.styles.xf(style), fonts[id])
        };

        // Bold: one `<b/>` replacing `<b val="0"/>`; underline, theme
        // colour and scheme kept.
        let (xf, font) = font_of(0);
        assert!(xf.bold, "{font}");
        assert_eq!(font.matches("<b").count(), 1, "{font}");
        assert!(font.contains("<b/>"), "{font}");
        assert!(font.contains("<u/>"), "{font}");
        assert!(font.contains("<color theme=\"1\"/>"), "{font}");
        assert!(font.contains("<scheme val=\"minor\"/>"), "{font}");
        assert!(!font.contains("<strike"), "{font}");

        // Colour: the theme colour replaced by the rgb one, underline kept.
        let (xf, font) = font_of(1);
        assert_eq!(xf.color, Some((0xC0, 0x00, 0x00)), "{font}");
        assert!(!xf.bold, "{font}");
        assert!(font.contains("<color rgb=\"FFC00000\"/>"), "{font}");
        assert!(!font.contains("theme="), "{font}");
        assert!(font.contains("<u/>"), "{font}");

        // Name: the new name, the old family and scheme gone, the rest kept.
        let (xf, font) = font_of(2);
        assert_eq!(xf.font_name.as_deref(), Some("Arial"), "{font}");
        assert!(font.contains("<name val=\"Arial\"/>"), "{font}");
        assert!(!font.contains("<family"), "{font}");
        assert!(!font.contains("<scheme"), "{font}");
        assert!(font.contains("<u/>"), "{font}");
        assert!(font.contains("<color theme=\"1\"/>"), "{font}");
    }

    #[test]
    fn an_alignment_the_model_cannot_name_survives_an_entry() {
        let mut pkg = new_xlsx();
        let xml = String::from_utf8(pkg.part("xl/styles.xml").unwrap().to_vec()).unwrap();
        let mut xml = bump_count(&xml, "<cellXfs", 1);
        xml = xml.replacen(
            "</cellXfs>",
            "<xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyAlignment=\"1\"><alignment horizontal=\"centerContinuous\" wrapText=\"1\"/></xf></cellXfs>",
            1,
        );
        pkg.set_part("xl/styles.xml", xml.into_bytes());
        let mut pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let src = pkg.workbook.styles.xfs.len() as u32 - 1;
        pkg.workbook.sheets[0].set_cell(
            0,
            0,
            crate::sheet::Cell {
                style: src,
                ..crate::sheet::Cell::default()
            },
        );
        let cell = crate::entry::entry_cell(&mut pkg.workbook, 0, 0, 0, "'x", None).unwrap();
        pkg.workbook.sheets[0].set_cell(0, 0, cell);
        // Right-aligning it on purpose replaces the horizontal, keeps the wrap.
        let mut right = pkg.workbook.styles.xf(src);
        right.align = crate::sheet::Align::Right;
        let r = pkg.workbook.styles.intern(right);
        pkg.workbook.sheets[0].set_cell(
            1,
            0,
            crate::sheet::Cell {
                style: r,
                ..crate::sheet::Cell::text("r")
            },
        );
        let saved = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let styles = String::from_utf8(saved.part("xl/styles.xml").unwrap().to_vec()).unwrap();
        let xfs = cell_xf_elements(&styles);
        let x = xfs[saved.workbook.sheets[0].cell(0, 0).unwrap().style as usize];
        assert!(x.contains("horizontal=\"centerContinuous\""), "{x}");
        assert!(
            x.contains("wrapText=\"1\"") && x.contains("quotePrefix=\"1\""),
            "{x}"
        );
        let x = xfs[saved.workbook.sheets[0].cell(1, 0).unwrap().style as usize];
        assert!(
            x.contains("horizontal=\"right\"") && !x.contains("centerContinuous"),
            "{x}"
        );
        assert!(x.contains("wrapText=\"1\""), "{x}");
    }

    #[test]
    fn quote_prefix_round_trips_through_styles() {
        let mut pkg = new_xlsx();
        let cell = crate::entry::entry_cell(&mut pkg.workbook, 0, 0, 0, "'007", None).unwrap();
        assert!(pkg.workbook.styles.xf(cell.style).quote_prefix);
        pkg.workbook.sheets[0].set_cell(0, 0, cell);
        let bytes = save_xlsx(&pkg);
        let re = load_xlsx(&bytes).unwrap();
        let cell = re.workbook.sheets[0].cell(0, 0).unwrap();
        assert_eq!(cell.value, crate::sheet::CellValue::Text("007".into()));
        assert!(re.workbook.styles.xf(cell.style).quote_prefix);
        let styles = String::from_utf8(re.part("xl/styles.xml").unwrap().to_vec()).unwrap();
        assert!(styles.contains("quotePrefix=\"1\""), "{styles}");
        let shared = String::from_utf8(re.part("xl/sharedStrings.xml").unwrap().to_vec())
            .unwrap_or_default();
        let sheet =
            String::from_utf8(re.part(&re.sheet_parts[0].clone()).unwrap().to_vec()).unwrap();
        assert!(!shared.contains("'007") && !sheet.contains("'007"));
        assert!(
            shared.contains(">007<") || sheet.contains(">007<"),
            "{shared}{sheet}"
        );
        // Saved again untouched, the loaded quote-prefixed xf stays as it was.
        let again = load_xlsx(&save_xlsx(&re)).unwrap();
        let c = again.workbook.sheets[0].cell(0, 0).unwrap();
        assert!(again.workbook.styles.xf(c.style).quote_prefix);
    }

    #[test]
    fn wrap_text_and_row_height_round_trip() {
        use crate::sheet::{Align, Cell, CellValue, Xf};
        let mut pkg = new_xlsx();
        // A cell with wrapText, and a wrap over an existing horizontal align.
        let idx = pkg.workbook.styles.intern(Xf {
            wrap: true,
            align: Align::Center,
            ..Default::default()
        });
        pkg.workbook.sheets[0].set_cell(
            0,
            0,
            Cell {
                value: CellValue::Text("a long wrapped label".into()),
                style: idx,
                ..Cell::default()
            },
        );
        pkg.workbook.sheets[0].set_row_height(0, Some(42.0));
        assert_eq!(pkg.workbook.sheets[0].row_height(0), Some(42.0));

        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let cell = re.workbook.sheets[0].cell(0, 0).unwrap();
        let xf = re.workbook.styles.xf(cell.style);
        assert!(xf.wrap, "wrapText must survive");
        assert_eq!(
            xf.align,
            Align::Center,
            "horizontal align kept alongside wrap"
        );
        assert_eq!(re.workbook.sheets[0].row_height(0), Some(42.0));
        let ws = String::from_utf8(re.part(&re.sheet_parts[0].clone()).unwrap().to_vec()).unwrap();
        assert!(ws.contains("ht=\"42\""), "{ws}");
        // Clearing the height drops the attrs.
        let mut re = re;
        re.workbook.sheets[0].set_row_height(0, None);
        assert_eq!(re.workbook.sheets[0].row_height(0), None);
    }

    #[test]
    fn text_with_special_chars_round_trips() {
        let mut pkg = new_xlsx();
        let tricky = "a<b & \"c\" > d\u{00e9}";
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::text(tricky));
        pkg.workbook.sheets[0].set_cell(
            1,
            0,
            Cell {
                value: CellValue::Text("x<y".into()),
                formula: Some("IF(A1<>\"\",\"x<y\",\"\")".to_string()),
                ..Cell::default()
            },
        );
        pkg.workbook.sheets[0].set_cell(2, 0, Cell::text("  padded  "));
        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).expect("reload");
        let s = &pkg2.workbook.sheets[0];
        assert_eq!(s.cell(0, 0).unwrap().value, CellValue::Text(tricky.into()));
        assert_eq!(
            s.cell(1, 0).unwrap().formula.as_deref(),
            Some("IF(A1<>\"\",\"x<y\",\"\")")
        );
        assert_eq!(
            s.cell(2, 0).unwrap().value,
            CellValue::Text("  padded  ".into())
        );
    }

    #[test]
    fn shared_strings_at_nonstandard_path_update_in_place() {
        // A workbook whose shared-strings part lives at an unconventional path
        // (referenced via the workbook rel) must be appended to *there* — not
        // duplicated at the standard xl/sharedStrings.xml, which would leave the
        // real (stale) part orphaned and the new string unreadable.
        let ct = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/strings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/></Types>"#;
        let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let workbook = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        // sharedStrings target is the non-standard "strings.xml".
        let wb_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="strings.xml"/></Relationships>"#;
        let sheet1 = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData></worksheet>"#;
        let strings = r#"<?xml version="1.0"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1"><si><t>orig</t></si></sst>"#;
        let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf/></cellStyleXfs><cellXfs count="1"><xf numFmtId="0" fontId="0"/></cellXfs></styleSheet>"#;
        let data = write_zip(&[
            ("[Content_Types].xml".into(), ct.into()),
            ("_rels/.rels".into(), root_rels.into()),
            ("xl/workbook.xml".into(), workbook.into()),
            ("xl/_rels/workbook.xml.rels".into(), wb_rels.into()),
            ("xl/worksheets/sheet1.xml".into(), sheet1.into()),
            ("xl/strings.xml".into(), strings.into()),
            ("xl/styles.xml".into(), styles.into()),
        ]);

        let mut pkg = load_xlsx(&data).expect("load nonstandard-sst workbook");
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 0).unwrap().value,
            CellValue::Text("orig".into())
        );
        // Add a new string, forcing an append to the shared-strings table.
        pkg.workbook.sheets[0].set_cell(1, 0, Cell::text("added"));
        let bytes = save_xlsx(&pkg);

        let pkg2 = load_xlsx(&bytes).expect("reload after save");
        assert_eq!(
            pkg2.workbook.sheets[0].cell(0, 0).unwrap().value,
            CellValue::Text("orig".into())
        );
        assert_eq!(
            pkg2.workbook.sheets[0].cell(1, 0).unwrap().value,
            CellValue::Text("added".into())
        );
        // The custom part was updated in place; no duplicate standard part was made.
        let names = pkg2.part_names();
        assert!(names.contains(&"xl/strings.xml"), "custom sst part missing");
        assert!(
            !names.contains(&"xl/sharedStrings.xml"),
            "a duplicate standard sharedStrings part was created"
        );
    }

    #[test]
    fn sheet_rename_persists() {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].name = "Budget & Plans".to_string();
        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).expect("reload");
        assert_eq!(pkg2.workbook.sheets[0].name, "Budget & Plans");
    }

    #[test]
    fn legacy_xls_is_rejected_with_hint() {
        let mut ole = OLE2.to_vec();
        ole.extend_from_slice(&[0u8; 100]);
        assert_eq!(load_xlsx(&ole).err(), Some(XlsxError::LegacyXls));
        assert_eq!(load_xlsx(b"not a zip").err(), Some(XlsxError::NotZip));
    }

    #[test]
    fn add_and_remove_sheets() {
        let mut pkg = new_xlsx();
        let idx = pkg.add_sheet("Report & Co");
        assert_eq!(idx, 1);
        pkg.workbook.sheets[1].set_cell(0, 0, Cell::text("hi"));
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(5.0));
        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).expect("reload with added sheet");
        assert_eq!(pkg2.workbook.sheets.len(), 2);
        assert_eq!(pkg2.workbook.sheets[1].name, "Report & Co");
        assert_eq!(
            pkg2.workbook.sheets[1].cell(0, 0).unwrap().value,
            CellValue::Text("hi".into())
        );
        // Removing the first sheet keeps the second intact.
        let mut pkg3 = pkg2.clone();
        assert!(pkg3.remove_sheet(0));
        let bytes = save_xlsx(&pkg3);
        let pkg4 = load_xlsx(&bytes).expect("reload after removal");
        assert_eq!(pkg4.workbook.sheets.len(), 1);
        assert_eq!(pkg4.workbook.sheets[0].name, "Report & Co");
        // The last sheet cannot be removed.
        let mut pkg5 = pkg4.clone();
        assert!(!pkg5.remove_sheet(0));
    }

    /// PERSISTENCE PROBE (Wave-3 Task 3, Part B): build a pivot the way the
    /// TUI's `create_pivot_from` does — source rows on Sheet1, `add_pivot`
    /// onto a fresh sheet, a row/col/value layout via `refresh_pivots` — then
    /// `save_xlsx` → `load_xlsx` and assert the definition survives in
    /// `workbook.pivots` AND `refresh_pivots` recomputes on the reload.
    #[test]
    fn pivot_survives_save_load_round_trip() {
        let mut pkg = new_xlsx();
        // Source data: Region/Product/Sales, header + 4 rows (Sheet1).
        let rows: [[&str; 3]; 5] = [
            ["Region", "Product", "Sales"],
            ["East", "Widget", "10"],
            ["East", "Gadget", "20"],
            ["West", "Widget", "30"],
            ["West", "Gadget", "40"],
        ];
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let cell = if r == 0 {
                    Cell::text(v)
                } else if c == 2 {
                    Cell::number(v.parse().unwrap())
                } else {
                    Cell::text(v)
                };
                pkg.workbook.sheets[0].set_cell(r as u32, c as u32, cell);
            }
        }
        let frame = crate::frame::Frame::from_range(&pkg.workbook, 0, (0, 0, 4, 2));
        assert_eq!(frame.names, vec!["Region", "Product", "Sales"]);
        let measure = crate::pivot::DataField {
            name: "Sum of Sales".into(),
            field: 2,
            agg: crate::frame::Agg::Sum,
        };
        let dest = pkg.add_sheet("Pivot");
        let idx = pkg
            .add_pivot(
                crate::pivot::PivotSource::Range {
                    sheet: "Sheet1".into(),
                    rect: (0, 0, 4, 2),
                },
                frame.names.clone(),
                measure,
                dest,
                (2, 0),
            )
            .expect("add_pivot");
        // Mirror the field editor: Region on rows (like Ctrl-P, 'r').
        pkg.workbook.pivots[idx].row_fields = vec![0];
        pkg.workbook.pivots[idx].edited = true;
        let outcome = crate::pivot::refresh_pivots(&mut pkg.workbook);
        assert_eq!(outcome.refreshed, 1, "refresh before save should succeed");
        // Sanity: the output sheet actually holds computed values pre-save.
        let sum_before = pkg.workbook.sheets[dest]
            .cell(3, 1)
            .map(|c| c.value.clone());
        assert!(
            matches!(sum_before, Some(CellValue::Number(_))),
            "expected a computed value on the pivot output sheet, got {sum_before:?}"
        );

        let bytes = save_xlsx(&pkg);
        let mut pkg2 = load_xlsx(&bytes).expect("reload with pivot");

        assert_eq!(
            pkg2.workbook.pivots.len(),
            1,
            "pivot definition lost on reload"
        );
        let p2 = &pkg2.workbook.pivots[0];
        assert_eq!(p2.row_fields, vec![0], "row layout lost on reload");
        assert_eq!(p2.data_fields.len(), 1);
        assert_eq!(p2.data_fields[0].agg, crate::frame::Agg::Sum);
        assert!(!p2.unsupported, "pivot round-tripped as unsupported");

        let outcome2 = crate::pivot::refresh_pivots(&mut pkg2.workbook);
        assert_eq!(
            outcome2.refreshed, 1,
            "refresh_pivots must recompute after reload"
        );
        let sum_after = pkg2.workbook.sheets[dest]
            .cell(3, 1)
            .map(|c| c.value.clone());
        assert_eq!(
            sum_before, sum_after,
            "recomputed pivot values differ after round-trip"
        );
    }

    /// A blank workbook with Region/Product/Sales data on Sheet1 (header +
    /// 4 rows), for `create_pivot`/`remove_pivot` tests.
    fn pkg_with_sales_data() -> SheetPackage {
        let mut pkg = new_xlsx();
        let rows: [[&str; 3]; 5] = [
            ["Region", "Product", "Sales"],
            ["East", "Widget", "10"],
            ["East", "Gadget", "20"],
            ["West", "Widget", "30"],
            ["West", "Gadget", "40"],
        ];
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let cell = if r > 0 && c == 2 {
                    Cell::number(v.parse().unwrap())
                } else {
                    Cell::text(v)
                };
                pkg.workbook.sheets[0].set_cell(r as u32, c as u32, cell);
            }
        }
        pkg
    }

    #[test]
    fn create_pivot_builds_full_layout_and_computes_on_a_new_sheet() {
        let mut pkg = pkg_with_sales_data();
        let frame = crate::frame::Frame::from_range(&pkg.workbook, 0, (0, 0, 4, 2));
        let spec = crate::frame::pivot_spec_from_names(
            &frame,
            &["Region".to_string()],
            &[],
            &[("Sales".to_string(), crate::frame::Agg::Sum)],
        )
        .expect("spec");
        let idx = pkg
            .create_pivot(
                crate::pivot::PivotSource::Range {
                    sheet: "Sheet1".into(),
                    rect: (0, 0, 4, 2),
                },
                &frame,
                &spec,
                "Pivot1",
            )
            .expect("create_pivot");
        let piv = &pkg.workbook.pivots[idx];
        assert_eq!(piv.row_fields, vec![0]);
        assert!(piv.col_fields.is_empty());
        assert_eq!(piv.data_fields.len(), 1);
        assert_eq!(piv.data_fields[0].agg, crate::frame::Agg::Sum);
        let dest = piv.sheet;
        assert_eq!(pkg.workbook.sheets[dest].name, "Pivot1");
        assert_ne!(dest, 0, "pivot must land on a NEW sheet, not the source");
        // Already refreshed: the output sheet holds computed values. Row 2
        // is the location's header row ("Sum of Sales"); row 3 is the first
        // row group ("East").
        let east_sum = pkg.workbook.sheets[dest]
            .cell(3, 1)
            .map(|c| c.value.clone());
        assert_eq!(east_sum, Some(CellValue::Number(30.0)));

        // No headers/rows or no measures: a clean None, no partial sheet left.
        let empty_frame = crate::frame::Frame::default();
        let n_sheets = pkg.workbook.sheets.len();
        assert!(
            pkg.create_pivot(
                crate::pivot::PivotSource::Range {
                    sheet: "Sheet1".into(),
                    rect: (0, 0, 4, 2),
                },
                &empty_frame,
                &spec,
                "Pivot2",
            )
            .is_none()
        );
        assert_eq!(
            pkg.workbook.sheets.len(),
            n_sheets,
            "a failed create_pivot must not leave a dangling sheet"
        );
    }

    #[test]
    fn remove_pivot_is_the_exact_inverse_of_create_pivot() {
        let mut pkg = pkg_with_sales_data();
        let frame = crate::frame::Frame::from_range(&pkg.workbook, 0, (0, 0, 4, 2));
        let spec = crate::frame::pivot_spec_from_names(
            &frame,
            &["Region".to_string()],
            &[],
            &[("Sales".to_string(), crate::frame::Agg::Sum)],
        )
        .expect("spec");
        let idx = pkg
            .create_pivot(
                crate::pivot::PivotSource::Range {
                    sheet: "Sheet1".into(),
                    rect: (0, 0, 4, 2),
                },
                &frame,
                &spec,
                "Pivot1",
            )
            .expect("create_pivot");
        let before = pkg.clone();
        assert!(pkg.remove_pivot(idx));
        assert!(pkg.workbook.pivots.is_empty(), "pivot registration remains");
        // Save/load still succeeds — no dangling relationship/content-type
        // pointing at the parts remove_pivot dropped.
        let bytes = save_xlsx(&pkg);
        let reloaded = load_xlsx(&bytes).expect("reload after remove_pivot");
        assert!(reloaded.workbook.pivots.is_empty());
        // The output sheet itself is untouched by remove_pivot alone —
        // callers that also want the sheet gone call remove_sheet (which
        // cascades pivot removal itself; see the next test).
        assert_eq!(pkg.workbook.sheets.len(), before.workbook.sheets.len());

        assert!(!pkg.remove_pivot(0), "no pivots left to remove");
    }

    #[test]
    fn removing_a_pivots_sheet_cascades_the_pivot_registration_both_or_neither() {
        let mut pkg = pkg_with_sales_data();
        let frame = crate::frame::Frame::from_range(&pkg.workbook, 0, (0, 0, 4, 2));
        let spec = crate::frame::pivot_spec_from_names(
            &frame,
            &["Region".to_string()],
            &[],
            &[("Sales".to_string(), crate::frame::Agg::Sum)],
        )
        .expect("spec");
        let idx = pkg
            .create_pivot(
                crate::pivot::PivotSource::Range {
                    sheet: "Sheet1".into(),
                    rect: (0, 0, 4, 2),
                },
                &frame,
                &spec,
                "Pivot1",
            )
            .expect("create_pivot");
        let dest = pkg.workbook.pivots[idx].sheet;
        assert!(pkg.remove_sheet(dest));
        assert!(
            pkg.workbook.pivots.is_empty(),
            "sheet.remove-style cascade must also drop the pivot registration \
             (a sheet-only inverse leaving a dangling pivot entry is a defect)"
        );
        assert_eq!(pkg.workbook.sheets.len(), 1);
        // Round-trips clean: no orphaned pivot part refs survive the removal.
        let bytes = save_xlsx(&pkg);
        let reloaded = load_xlsx(&bytes).expect("reload after cascaded removal");
        assert!(reloaded.workbook.pivots.is_empty());
        assert_eq!(reloaded.workbook.sheets.len(), 1);
    }

    #[test]
    fn remove_sheet_drops_its_rels_and_unshared_targets() {
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let mut pkg = new_xlsx();
        pkg.add_sheet("Two");
        assert_eq!(pkg.sheet_parts[1], "xl/worksheets/sheet2.xml");
        // Sheet 1 has comments with their VML, and printer settings it shares
        // with sheet 2.
        pkg.set_part(
            "xl/worksheets/_rels/sheet1.xml.rels",
            format!(
                "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
                 <Relationship Id=\"rId1\" Type=\"{rel}/comments\" Target=\"../comments1.xml\"/>\
                 <Relationship Id=\"rId2\" Type=\"{rel}/vmlDrawing\" Target=\"../drawings/vmlDrawing1.vml\"/>\
                 <Relationship Id=\"rId3\" Type=\"{rel}/printerSettings\" Target=\"../printerSettings/printerSettings1.bin\"/>\
                 </Relationships>"
            )
            .into_bytes(),
        );
        pkg.set_part(
            "xl/worksheets/_rels/sheet2.xml.rels",
            format!(
                "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
                 <Relationship Id=\"rId1\" Type=\"{rel}/printerSettings\" Target=\"../printerSettings/printerSettings1.bin\"/>\
                 </Relationships>"
            )
            .into_bytes(),
        );
        pkg.set_part("xl/comments1.xml", b"<comments/>".to_vec());
        pkg.set_part("xl/drawings/vmlDrawing1.vml", b"<xml/>".to_vec());
        pkg.set_part("xl/printerSettings/printerSettings1.bin", vec![0; 4]);
        let ct = part_text(&pkg, "[Content_Types].xml").replace(
            "</Types>",
            "<Override PartName=\"/xl/comments1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml\"/></Types>",
        );
        pkg.set_part("[Content_Types].xml", ct.into_bytes());

        assert!(pkg.remove_sheet(0));
        for gone in [
            "xl/worksheets/sheet1.xml",
            "xl/worksheets/_rels/sheet1.xml.rels",
            "xl/comments1.xml",
            "xl/drawings/vmlDrawing1.vml",
        ] {
            assert!(pkg.part(gone).is_none(), "{gone} survived");
        }
        assert!(
            pkg.part("xl/printerSettings/printerSettings1.bin")
                .is_some()
        );
        let ct = part_text(&pkg, "[Content_Types].xml");
        assert!(
            !ct.contains("comments1") && !ct.contains("sheet1.xml"),
            "{ct}"
        );
        assert!(ct.contains("/xl/worksheets/sheet2.xml"), "{ct}");
        let saved = load_xlsx(&save_xlsx(&pkg)).expect("reload");
        assert_eq!(saved.workbook.sheets.len(), 1);
        assert_eq!(saved.workbook.sheets[0].name, "Two");
    }

    #[test]
    fn remove_sheet_drops_its_tables_and_shifts_later_ones() {
        let mut pkg = new_xlsx();
        pkg.add_sheet("Two");
        let t1 = pkg.add_table(0, (0, 0, 2, 1), true, "TableStyleLight1");
        let t2 = pkg.add_table(1, (0, 0, 2, 1), true, "TableStyleLight1");
        let (t1, t2) = (t1.unwrap(), t2.unwrap());
        let (p1, p2) = (
            pkg.workbook.tables[t1].part.clone(),
            pkg.workbook.tables[t2].part.clone(),
        );
        assert!(pkg.remove_sheet(0));
        assert_eq!(pkg.workbook.tables.len(), 1);
        assert_eq!(pkg.workbook.tables[0].sheet, 0);
        assert!(
            pkg.part(&p1).is_none(),
            "the removed sheet's table part stays"
        );
        assert!(pkg.part(&p2).is_some());
        let saved = load_xlsx(&save_xlsx(&pkg)).expect("reload");
        assert_eq!(saved.workbook.tables.len(), 1);
        assert_eq!(saved.workbook.tables[0].sheet, 0);
    }

    #[test]
    fn removing_an_unrelated_sheet_shifts_a_surviving_pivots_sheet_index() {
        // Pivot lands on sheet 1 (Sheet1=0, Pivot1=1). Adding a sheet BEFORE
        // it, then removing that inserted sheet, must shift the pivot's
        // `.sheet` back down rather than leaving it stale or cascading it
        // away (it wasn't the pivot's own sheet).
        let mut pkg = pkg_with_sales_data();
        let frame = crate::frame::Frame::from_range(&pkg.workbook, 0, (0, 0, 4, 2));
        let spec = crate::frame::pivot_spec_from_names(
            &frame,
            &["Region".to_string()],
            &[],
            &[("Sales".to_string(), crate::frame::Agg::Sum)],
        )
        .expect("spec");
        let idx = pkg
            .create_pivot(
                crate::pivot::PivotSource::Range {
                    sheet: "Sheet1".into(),
                    rect: (0, 0, 4, 2),
                },
                &frame,
                &spec,
                "Pivot1",
            )
            .expect("create_pivot");
        assert_eq!(pkg.workbook.pivots[idx].sheet, 1);
        // Remove sheet 0 (Sheet1, the source data — unrelated to the pivot's
        // OWN sheet at index 1) and check the pivot's index shifts to 0.
        assert!(pkg.remove_sheet(0));
        assert_eq!(
            pkg.workbook.pivots[0].sheet, 0,
            "surviving pivot's sheet index must shift down with the removal"
        );
        assert_eq!(pkg.workbook.sheets[0].name, "Pivot1");
    }

    #[test]
    fn refreshing_a_pivots_output_after_its_source_sheet_is_removed_skips_gracefully() {
        // Wave-3 Task 4's mandatory regression test: removing a pivot's
        // SOURCE sheet (its OWN output sheet survives, unlike the cascade
        // tests above) must leave the pivot registration in place — merely
        // stale, pointing at source data that no longer exists — and a
        // subsequent refresh (the `wb.recalc` path) must skip it gracefully
        // rather than panicking on the now-dangling `PivotSource::Range`
        // sheet name.
        use crate::pivot::refresh_pivots;
        let mut pkg = pkg_with_sales_data();
        let frame = crate::frame::Frame::from_range(&pkg.workbook, 0, (0, 0, 4, 2));
        let spec = crate::frame::pivot_spec_from_names(
            &frame,
            &["Region".to_string()],
            &[],
            &[("Sales".to_string(), crate::frame::Agg::Sum)],
        )
        .expect("spec");
        pkg.create_pivot(
            crate::pivot::PivotSource::Range {
                sheet: "Sheet1".into(),
                rect: (0, 0, 4, 2),
            },
            &frame,
            &spec,
            "Pivot1",
        )
        .expect("create_pivot");
        assert_eq!(pkg.workbook.pivots[0].sheet, 1, "Pivot1 is sheet index 1");

        // Remove sheet 0 (Sheet1 — the pivot's SOURCE, not its own output
        // sheet). The pivot's own sheet (now index 0) survives.
        assert!(pkg.remove_sheet(0));
        assert_eq!(
            pkg.workbook.pivots.len(),
            1,
            "pivot.list must still report the pivot — it's stale, not gone"
        );
        assert_eq!(pkg.workbook.sheets.len(), 1);
        assert_eq!(pkg.workbook.sheets[0].name, "Pivot1");

        // Refresh (the wb.recalc path) must not panic, and must skip this
        // pivot gracefully rather than crash resolving its now-nonexistent
        // "Sheet1" source.
        let outcome = refresh_pivots(&mut pkg.workbook);
        assert_eq!(outcome.refreshed, 0);
        assert_eq!(outcome.skipped, 1);
        assert!(outcome.changed.is_empty());
        // The pivot registration is still there afterward (stale, not
        // dropped by the failed refresh attempt).
        assert_eq!(pkg.workbook.pivots.len(), 1);
    }

    #[test]
    fn dimension_is_updated() {
        let mut pkg = load_xlsx(&fixture()).expect("load");
        pkg.workbook.sheets[0].set_cell(99, 25, Cell::number(1.0));
        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).expect("reload");
        let ws =
            String::from_utf8_lossy(pkg2.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
        assert!(ws.contains("<dimension ref=\"A1:Z100\"/>"), "{ws}");
    }

    #[test]
    fn spill_round_trips_as_array_formula() {
        // A workbook with a spilling anchor saves as <f t="array" ref="…">
        // and loads back with the extent on Cell::spill.
        let mut pkg = new_xlsx();
        // Enter the spill through the app path (eng.set_cell → modern formula),
        // as a user would; a plain sheet.set_cell would load back as legacy.
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.set_cell(
            &mut pkg.workbook,
            (0, 0, 0),
            crate::sheet::Cell::formula("SEQUENCE(3)"),
        );
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 0).unwrap().spill,
            Some((3, 1))
        );

        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).unwrap();
        let anchor = pkg2.workbook.sheets[0].cell(0, 0).unwrap();
        // Saved in file spelling (#776); shown as typed.
        assert_eq!(anchor.formula.as_deref(), Some("_xlfn.SEQUENCE(3)"));
        assert_eq!(
            crate::formula::display_formula(anchor.formula.as_deref().unwrap()),
            "SEQUENCE(3)"
        );
        assert_eq!(anchor.spill, Some((3, 1)));
        assert!(anchor.f_attrs.as_deref().unwrap().contains("t=\"array\""));
        assert!(anchor.f_attrs.as_deref().unwrap().contains("ref=\"A1:A3\""));
        // Spilled values persisted as plain cells…
        assert_eq!(
            pkg2.workbook.sheets[0].cell(2, 0).unwrap().value,
            crate::sheet::CellValue::Number(3.0)
        );
        // …and the loaded engine evaluates the anchor (not frozen) to the
        // same result.
        let mut pkg3 = load_xlsx(&bytes).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg3.workbook);
        assert!(!eng.is_unsupported((0, 0, 0)));
        eng.recalc_all(&mut pkg3.workbook);
        assert_eq!(
            pkg3.workbook.sheets[0].cell(1, 0).unwrap().value,
            crate::sheet::CellValue::Number(2.0)
        );
    }

    /// A one-sheet workbook with the standard dynamic-array `xl/metadata.xml`
    /// (`cm="1"` is XLDAPR `fDynamic`); `rows` is the `<sheetData>` content.
    fn cell_meta_fixture(rows: &str) -> Vec<u8> {
        let sheet = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{rows}</sheetData></worksheet>"#
        );
        let metadata = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<metadata xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xda="http://schemas.microsoft.com/office/spreadsheetml/2017/dynamicarray"><metadataTypes count="1"><metadataType name="XLDAPR" minSupportedVersion="120000" copy="1" pasteAll="1" pasteValues="1" merge="1" splitFirst="1" rowColShift="1" clearFormats="1" clearComments="1" assign="1" coerce="1" cellMeta="1"/></metadataTypes><futureMetadata name="XLDAPR" count="1"><bk><extLst><ext uri="{bdbb8cdc-fa1e-496e-a857-3c3f30c029c3}"><xda:dynamicArrayProperties fDynamic="1" fCollapsed="0"/></ext></extLst></bk></futureMetadata><cellMetadata count="1"><bk><rc t="1" v="0"/></bk></cellMetadata></metadata>"#;
        let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let wb_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sheetMetadata" Target="metadata.xml"/></Relationships>"#;
        let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/metadata.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheetMetadata+xml"/></Types>"#;
        write_zip(&[
            ("[Content_Types].xml".into(), content_types.into()),
            ("_rels/.rels".into(), root_rels.into()),
            ("xl/workbook.xml".into(), workbook.into()),
            ("xl/_rels/workbook.xml.rels".into(), wb_rels.into()),
            ("xl/worksheets/sheet1.xml".into(), sheet.into_bytes()),
            ("xl/metadata.xml".into(), metadata.into()),
        ])
    }

    /// Excel 2024's rich-error metadata: value-metadata entries 1..4 point at
    /// rich values 0..3; entry 5 is an XLDAPR entry, not a rich value.
    const RICH_METADATA: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<metadata xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xlrd="http://schemas.microsoft.com/office/spreadsheetml/2017/richdata"><metadataTypes count="2"><metadataType name="XLDAPR" minSupportedVersion="120000"/><metadataType name="XLRICHVALUE" minSupportedVersion="120000" copy="1" pasteAll="1" pasteValues="1" merge="1" splitFirst="1" rowColShift="1" clearFormats="1" clearComments="1" assign="1" coerce="1"/></metadataTypes><futureMetadata name="XLRICHVALUE" count="4"><bk><extLst><ext uri="{3e2802c4-a4d2-4d8b-9148-e3be6c30e623}"><xlrd:rvb i="0"/></ext></extLst></bk><bk><extLst><ext uri="{3e2802c4-a4d2-4d8b-9148-e3be6c30e623}"><xlrd:rvb i="1"/></ext></extLst></bk><bk><extLst><ext uri="{3e2802c4-a4d2-4d8b-9148-e3be6c30e623}"><xlrd:rvb i="2"/></ext></extLst></bk><bk><extLst><ext uri="{3e2802c4-a4d2-4d8b-9148-e3be6c30e623}"><xlrd:rvb i="3"/></ext></extLst></bk></futureMetadata><valueMetadata count="5"><bk><rc t="2" v="0"/></bk><bk><rc t="2" v="1"/></bk><bk><rc t="2" v="2"/></bk><bk><rc t="2" v="3"/></bk><bk><rc t="1" v="0"/></bk></valueMetadata></metadata>"#;
    /// Rich `_error` values: #SPILL! (8), #CALC! (13), #GETTING_DATA (7) and
    /// a plain #VALUE! (2).
    const RICH_VALUES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<rvData xmlns="http://schemas.microsoft.com/office/spreadsheetml/2017/richdata" count="4"><rv s="0"><v>0</v><v>8</v></rv><rv s="0"><v>0</v><v>13</v></rv><rv s="0"><v>0</v><v>7</v></rv><rv s="0"><v>0</v><v>2</v></rv></rvData>"#;
    /// The `_error` structure, with `errorType` second so it must be found
    /// by name.
    const RICH_STRUCTURES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<rvStructures xmlns="http://schemas.microsoft.com/office/spreadsheetml/2017/richdata" count="1"><s t="_error"><k n="propagated" t="b"/><k n="errorType" t="i"/></s></rvStructures>"#;

    /// A one-sheet workbook with `rows` and the rich-error parts above.
    fn rich_error_fixture(rows: &str) -> Vec<u8> {
        let sheet = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{rows}</sheetData></worksheet>"#
        );
        let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let wb_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sheetMetadata" Target="metadata.xml"/><Relationship Id="rId3" Type="http://schemas.microsoft.com/office/2017/06/relationships/rdRichValue" Target="richData/rdrichvalue.xml"/><Relationship Id="rId4" Type="http://schemas.microsoft.com/office/2017/06/relationships/rdRichValueStructure" Target="richData/rdrichvaluestructure.xml"/></Relationships>"#;
        let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/metadata.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheetMetadata+xml"/></Types>"#;
        write_zip(&[
            ("[Content_Types].xml".into(), content_types.into()),
            ("_rels/.rels".into(), root_rels.into()),
            ("xl/workbook.xml".into(), workbook.into()),
            ("xl/_rels/workbook.xml.rels".into(), wb_rels.into()),
            ("xl/worksheets/sheet1.xml".into(), sheet.into_bytes()),
            ("xl/metadata.xml".into(), RICH_METADATA.into()),
            ("xl/richData/rdrichvalue.xml".into(), RICH_VALUES.into()),
            (
                "xl/richData/rdrichvaluestructure.xml".into(),
                RICH_STRUCTURES.into(),
            ),
        ])
    }

    /// A1..A5 are `<v>#VALUE!</v>` with vm 1..5, standing for #SPILL!,
    /// #CALC!, #GETTING_DATA, a plain #VALUE! and nothing; B1..B3 take
    /// ERROR.TYPE of A1..A3.
    const RICH_ROWS: &str = r#"<row r="1"><c r="A1" t="e" vm="1"><v>#VALUE!</v></c><c r="B1"><f>ERROR.TYPE(A1)</f><v>3</v></c></row><row r="2"><c r="A2" t="e" vm="2"><v>#VALUE!</v></c><c r="B2"><f>ERROR.TYPE(A2)</f><v>3</v></c></row><row r="3"><c r="A3" t="e" vm="3"><v>#VALUE!</v></c><c r="B3"><f>ERROR.TYPE(A3)</f><v>3</v></c></row><row r="4"><c r="A4" t="e" vm="4"><v>#VALUE!</v></c></row><row r="5"><c r="A5" t="e" vm="5"><v>#VALUE!</v></c></row>"#;

    #[test]
    fn rich_errors_decode_from_value_metadata() {
        // #657 (a): the real error comes from the rich value, and ERROR.TYPE
        // sees it.
        let mut pkg = load_xlsx(&rich_error_fixture(RICH_ROWS)).unwrap();
        let v =
            |pkg: &SheetPackage, r: u32| pkg.workbook.sheets[0].cell(r, 0).unwrap().value.clone();
        let err = |s: &str| CellValue::Error(s.into());
        assert_eq!(v(&pkg, 0), err("#SPILL!"));
        assert_eq!(v(&pkg, 1), err("#CALC!"));
        assert_eq!(v(&pkg, 2), err("#GETTING_DATA"));
        assert_eq!(v(&pkg, 3), err("#VALUE!"));
        // vm 5 is not a rich value: left alone.
        assert_eq!(v(&pkg, 4), err("#VALUE!"));
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let b = |r: u32| pkg.workbook.sheets[0].cell(r, 1).unwrap().value.clone();
        assert_eq!(b(0), CellValue::Number(9.0));
        assert_eq!(b(1), CellValue::Number(14.0));
        assert_eq!(b(2), CellValue::Number(8.0));
    }

    #[test]
    fn rich_errors_save_in_the_files_form() {
        // #657 (b): while the value is unchanged, save writes `vm` and the
        // file's `#VALUE!` body, after a recalc too, and leaves the metadata
        // parts alone.
        let mut pkg = load_xlsx(&rich_error_fixture(RICH_ROWS)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let ws = saved_sheet1(&pkg);
        for (r, vm) in [(1, 1), (2, 2), (3, 3), (4, 4), (5, 5)] {
            let want = format!(r#"<c r="A{r}" t="e" vm="{vm}"><v>#VALUE!</v></c>"#);
            assert!(ws.contains(&want), "{want} in {ws}");
        }
        let saved = load_xlsx(&save_xlsx(&pkg)).unwrap();
        for (part, body) in [
            ("xl/metadata.xml", RICH_METADATA),
            ("xl/richData/rdrichvalue.xml", RICH_VALUES),
            ("xl/richData/rdrichvaluestructure.xml", RICH_STRUCTURES),
        ] {
            assert_eq!(saved.part(part), Some(body.as_bytes()), "{part}");
        }
        // Reloading decodes them again.
        assert_eq!(
            saved.workbook.sheets[0].cell(0, 0).unwrap().value,
            CellValue::Error("#SPILL!".into())
        );

        // (c): a changed value drops `vm` and writes its own code.
        for (r, code) in [(0, "#CALC!"), (1, "#N/A")] {
            let mut cell = pkg.workbook.sheets[0].cell(r, 0).unwrap().clone();
            cell.value = CellValue::Error(code.into());
            pkg.workbook.sheets[0].set_cell(r, 0, cell);
        }
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<c r="A1" t="e"><v>#CALC!</v></c>"#), "{ws}");
        assert!(ws.contains(r#"<c r="A2" t="e"><v>#N/A</v></c>"#), "{ws}");
        assert!(
            ws.contains(r#"<c r="A3" t="e" vm="3"><v>#VALUE!</v></c>"#),
            "{ws}"
        );
    }

    #[test]
    fn spill_and_calc_are_not_formula_constants() {
        // #657: Excel refuses `=#SPILL!` and `=#CALC!` as formulas but takes
        // `#GETTING_DATA`.
        use crate::formula::parse;
        for src in [
            "#SPILL!",
            "#CALC!",
            "ERROR.TYPE(#SPILL!)",
            "ERROR.TYPE(#CALC!)",
        ] {
            assert!(parse(src).is_err(), "{src}");
        }
        assert!(parse("ERROR.TYPE(#GETTING_DATA)").is_ok());
        assert!(parse("ERROR.TYPE(#N/A)").is_ok());
        let wb_err = crate::engine::eval_formula_at(
            &load_xlsx(&rich_error_fixture("")).unwrap().workbook,
            0,
            9,
            9,
            "ERROR.TYPE(#GETTING_DATA)",
        );
        assert_eq!(wb_err, crate::formula::Value::Num(8.0));
        // A typed constant is still the error value.
        assert_eq!(
            crate::edit::parse_input("#SPILL!").value,
            CellValue::Error("#SPILL!".into())
        );
        assert_eq!(
            crate::edit::parse_input("#GETTING_DATA").value,
            CellValue::Error("#GETTING_DATA".into())
        );
    }

    /// The #678 list on Sheet1 (A1:C9: Region, Rep, Amount), rows 3, 5, 6,
    /// 7 and 9 hidden, plus `filter` after `</sheetData>`, and `sheet2` as
    /// Sheet2's rows. With `table`, Sheet1 also carries a table part
    /// `Sales` over A1:C9 holding that autoFilter instead.
    fn filter_fixture(filter: &str, sheet2: &str, table: bool) -> Vec<u8> {
        let recs = [
            ("East", "Ann", 10),
            ("West", "Bob", 20),
            ("East", "Cy", 40),
            ("North", "Di", 80),
            ("East", "Ed", 160),
            ("West", "Fa", 320),
            ("East", "Gu", 640),
            ("South", "Hu", 1280),
        ];
        let is = |r: &str, t: &str| format!(r#"<c r="{r}" t="inlineStr"><is><t>{t}</t></is></c>"#);
        let mut rows = format!(
            "<row r=\"1\">{}{}{}</row>",
            is("A1", "Region"),
            is("B1", "Rep"),
            is("C1", "Amount")
        );
        for (i, (region, rep, amt)) in recs.iter().enumerate() {
            let r = i + 2;
            let hidden = if [3, 5, 6, 7, 9].contains(&r) {
                r#" hidden="1""#
            } else {
                ""
            };
            rows.push_str(&format!(
                r#"<row r="{r}"{hidden}>{}{}<c r="C{r}"><v>{amt}</v></c></row>"#,
                is(&format!("A{r}"), region),
                is(&format!("B{r}"), rep)
            ));
        }
        let parts_tag = if table {
            r#"<tableParts count="1"><tablePart r:id="rId1"/></tableParts>"#
        } else {
            ""
        };
        let sheet1 = format!(
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData>{rows}</sheetData>{}{parts_tag}</worksheet>"#,
            if table { "" } else { filter }
        );
        let sheet2 = format!(
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{sheet2}</sheetData></worksheet>"#
        );
        let table_xml = format!(
            r#"<table xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" id="1" name="Sales" displayName="Sales" ref="A1:C9">{filter}<tableColumns count="3"><tableColumn id="1" name="Region"/><tableColumn id="2" name="Rep"/><tableColumn id="3" name="Amount"/></tableColumns></table>"#
        );
        let workbook = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/><sheet name="Sheet2" sheetId="2" r:id="rId2"/></sheets></workbook>"#;
        let wb_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#;
        let ws_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/table" Target="../tables/table1.xml"/></Relationships>"#;
        let root_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let content_types = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;
        let mut parts: Vec<(String, Vec<u8>)> = vec![
            ("[Content_Types].xml".into(), content_types.into()),
            ("_rels/.rels".into(), root_rels.into()),
            ("xl/workbook.xml".into(), workbook.into()),
            ("xl/_rels/workbook.xml.rels".into(), wb_rels.into()),
            ("xl/worksheets/sheet1.xml".into(), sheet1.into_bytes()),
            ("xl/worksheets/sheet2.xml".into(), sheet2.into_bytes()),
        ];
        if table {
            parts.push(("xl/worksheets/_rels/sheet1.xml.rels".into(), ws_rels.into()));
            parts.push(("xl/tables/table1.xml".into(), table_xml.into_bytes()));
        }
        write_zip(&parts)
    }

    /// The #678 filter: Region = East.
    const EAST_FILTER: &str = r#"<autoFilter ref="A1:C9"><filterColumn colId="0"><filters><filter val="East"/></filters></filterColumn></autoFilter>"#;

    /// Recalculate the workbook and read `cells` on sheet `sheet`.
    fn recalc_values(pkg: &mut SheetPackage, sheet: usize, cells: &[&str]) -> Vec<CellValue> {
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        cells
            .iter()
            .map(|c| {
                let (r, col) = parse_cell_name(c).unwrap();
                pkg.workbook.sheets[sheet]
                    .cell(r, col)
                    .unwrap()
                    .value
                    .clone()
            })
            .collect()
    }

    #[test]
    fn subtotal_counts_hand_hidden_rows_under_a_filter() {
        // #678: the issue's workbook. Region = East filters out rows 3, 5, 7
        // and 9; row 6 (Ed, East) is hidden by hand.
        let formulas = r#"<row r="1"><c r="A1"><f>SUBTOTAL(9,Sheet1!C2:C9)</f><v>0</v></c><c r="B1"><f>SUBTOTAL(3,Sheet1!A2:A9)</f><v>0</v></c><c r="C1"><f>SUBTOTAL(1,Sheet1!C2:C9)</f><v>0</v></c><c r="D1"><f>SUBTOTAL(109,Sheet1!C2:C9)</f><v>0</v></c><c r="E1"><f>SUBTOTAL(103,Sheet1!A2:A9)</f><v>0</v></c><c r="F1"><f>AGGREGATE(9,5,Sheet1!C2:C9)</f><v>0</v></c></row>"#;
        let mut pkg = load_xlsx(&filter_fixture(EAST_FILTER, formulas, false)).unwrap();
        let s1 = &pkg.workbook.sheets[0];
        assert_eq!(
            s1.filtered_rows.iter().copied().collect::<Vec<_>>(),
            vec![2, 4, 6, 8]
        );
        assert!(!s1.row_filtered(5) && s1.row_hidden(5));
        let got = recalc_values(&mut pkg, 1, &["A1", "B1", "C1", "D1", "E1", "F1"]);
        let n = CellValue::Number;
        assert_eq!(
            got,
            vec![n(850.0), n(4.0), n(212.5), n(690.0), n(3.0), n(690.0)]
        );
    }

    #[test]
    fn subtotal_over_a_filtered_table_counts_hand_hidden_rows() {
        // #678: the same list as a table whose own autoFilter did the
        // filtering.
        let formulas = r#"<row r="1"><c r="A1"><f>SUBTOTAL(9,Sales[Amount])</f><v>0</v></c><c r="B1"><f>SUBTOTAL(109,Sales[Amount])</f><v>0</v></c></row>"#;
        let mut pkg = load_xlsx(&filter_fixture(EAST_FILTER, formulas, true)).unwrap();
        let got = recalc_values(&mut pkg, 1, &["A1", "B1"]);
        assert_eq!(
            got,
            vec![CellValue::Number(850.0), CellValue::Number(690.0)]
        );
    }

    #[test]
    fn filtered_rows_from_custom_and_unsupported_filters() {
        // Amount >= 100 AND < 1000 keeps rows 6 (160) and 8 (640) of the
        // hidden ones visible-worthy: hidden 3/5/7/9 fail or pass by value.
        let custom = r#"<autoFilter ref="A1:C9"><filterColumn colId="2"><customFilters and="1"><customFilter operator="greaterThanOrEqual" val="100"/><customFilter operator="lessThan" val="1000"/></customFilters></filterColumn></autoFilter>"#;
        let pkg = load_xlsx(&filter_fixture(custom, "", false)).unwrap();
        // Hidden rows 3 (20), 5 (80), 6 (160), 7 (320), 9 (1280): 160 and 320
        // pass, so rows 6 and 7 were hidden by hand.
        assert_eq!(
            pkg.workbook.sheets[0]
                .filtered_rows
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![2, 4, 8]
        );
        // Wildcards in a custom equal filter.
        let wild = r#"<autoFilter ref="A1:C9"><filterColumn colId="1"><customFilters><customFilter val="F*"/><customFilter val="E?"/></customFilters></filterColumn></autoFilter>"#;
        let pkg = load_xlsx(&filter_fixture(wild, "", false)).unwrap();
        // Rows 6 (Ed) and 7 (Fa) pass.
        assert_eq!(
            pkg.workbook.sheets[0]
                .filtered_rows
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![2, 4, 8]
        );
        // A filter we cannot re-evaluate: every hidden row counts as filtered.
        let top = r#"<autoFilter ref="A1:C9"><filterColumn colId="2"><top10 val="3"/></filterColumn></autoFilter>"#;
        let pkg = load_xlsx(&filter_fixture(top, "", false)).unwrap();
        assert_eq!(
            pkg.workbook.sheets[0]
                .filtered_rows
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![2, 4, 5, 6, 8]
        );
        // A column with no criteria (a hidden button) filters nothing; the
        // East column still decides.
        let buttons = r#"<autoFilter ref="A1:C9"><filterColumn colId="1" hiddenButton="1"/><filterColumn colId="2" showButton="0"/><filterColumn colId="0"><filters><filter val="East"/></filters></filterColumn></autoFilter>"#;
        let pkg = load_xlsx(&filter_fixture(buttons, "", false)).unwrap();
        assert_eq!(
            pkg.workbook.sheets[0]
                .filtered_rows
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![2, 4, 6, 8]
        );
        // No autoFilter: every hidden row was hidden by hand.
        let pkg = load_xlsx(&filter_fixture("", "", false)).unwrap();
        assert!(pkg.workbook.sheets[0].filtered_rows.is_empty());
    }

    #[test]
    fn filtered_rows_follow_row_edits_and_unhide() {
        // #678: inserting a row above the list moves the filtered rows with
        // it, and a filtered row unhidden by hand counts again.
        let formulas = r#"<row r="1"><c r="A1"><f>SUBTOTAL(9,Sheet1!C2:C10)</f><v>0</v></c></row>"#;
        let mut pkg = load_xlsx(&filter_fixture(EAST_FILTER, formulas, false)).unwrap();
        crate::edit::insert_rows(&mut pkg.workbook, 0, 1, 1);
        assert_eq!(
            pkg.workbook.sheets[0]
                .filtered_rows
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![3, 5, 7, 9]
        );
        let got = recalc_values(&mut pkg, 1, &["A1"]);
        assert_eq!(got, vec![CellValue::Number(850.0)]);
        // Unhide Bob (row 3, now 4): he counts in SUBTOTAL(9) again.
        pkg.workbook.sheets[0].set_row_hidden(3, false);
        let got = recalc_values(&mut pkg, 1, &["A1"]);
        assert_eq!(got, vec![CellValue::Number(870.0)]);
    }

    /// Rows 1..=n with A holding 3, 9, 1, 7, 5, 2, 8 (the first `n`), and
    /// `anchor` appended to row 1.
    fn sort_anchor_rows(n: usize, anchor: &str) -> String {
        [3, 9, 1, 7, 5, 2, 8][..n]
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let extra = if i == 0 { anchor } else { "" };
                format!(
                    r#"<row r="{0}"><c r="A{0}"><v>{v}</v></c>{extra}</row>"#,
                    i + 1
                )
            })
            .collect()
    }

    /// The anchor exactly as the issue (#598) reports Excel writing it.
    const SORT_ANCHOR: &str =
        r#"<c r="D1" cm="1"><f t="array" ref="D1:D5">_xlfn._xlws.SORT(A1:A5,,-1)</f><v>9</v></c>"#;

    fn saved_sheet1(pkg: &SheetPackage) -> String {
        let reloaded = load_xlsx(&save_xlsx(pkg)).unwrap();
        String::from_utf8_lossy(reloaded.part("xl/worksheets/sheet1.xml").unwrap()).into_owned()
    }

    #[test]
    fn dynamic_array_cm_survives_load_and_save() {
        let pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        let bytes = save_xlsx(&pkg);
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(SORT_ANCHOR), "{ws}");
        assert!(load_xlsx(&bytes).unwrap().part("xl/metadata.xml").is_some());
    }

    #[test]
    fn dynamic_array_cm_survives_full_recalc() {
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(SORT_ANCHOR), "{ws}");
    }

    #[test]
    fn dynamic_array_cm_survives_a_changed_spill_extent() {
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(7, SORT_ANCHOR))).unwrap();
        // The source grows to A1:A7, so the spill grows to D1:D7.
        pkg.workbook.sheets[0]
            .cells
            .get_mut(&(0, 3))
            .unwrap()
            .formula = Some("_xlfn._xlws.SORT(A1:A7,,-1)".into());
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1" cm="1"><f t="array" ref="D1:D7">_xlfn._xlws.SORT(A1:A7,,-1)</f><v>9</v></c>"#),
            "{ws}"
        );
    }

    #[test]
    fn format_edit_through_set_cell_keeps_cm() {
        // xlsxy formats a cell by cloning it and changing only the style.
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        let mut cell = pkg.workbook.sheets[0].cell(0, 3).cloned().unwrap();
        cell.style = 1;
        eng.set_cell(&mut pkg.workbook, (0, 0, 3), cell);
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 3).unwrap().spill,
            Some((5, 1))
        );
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1" s="1" cm="1"><f t="array" ref="D1:D5">"#),
            "{ws}"
        );
    }

    #[test]
    fn typed_formula_reuses_existing_dynamic_cm() {
        // #724: a typed formula the engine evaluates as an array is a dynamic
        // array. This file already has the XLDAPR `fDynamic` entry (`cm="1"`):
        // the typed formula uses it, and the part is left exactly as it was.
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        let before = pkg.part("xl/metadata.xml").unwrap().to_vec();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 0, 3), Cell::formula("SEQUENCE(2)"));
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let ws = String::from_utf8_lossy(re.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
        assert!(
            ws.contains(r#"<c r="D1" cm="1"><f t="array" ref="D1:D2">_xlfn.SEQUENCE(2)</f>"#),
            "{ws}"
        );
        assert_eq!(re.part("xl/metadata.xml").unwrap(), &before[..]);
    }

    /// Save `pkg`, reload it, and return (package, sheet1 XML).
    fn resaved(pkg: &SheetPackage) -> (SheetPackage, String) {
        let re = load_xlsx(&save_xlsx(pkg)).unwrap();
        let ws = String::from_utf8_lossy(re.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
        (re, ws)
    }

    /// A new workbook with 1, 2, 3 in A1:A3, its engine, and `typed` entered
    /// (as xlsxy commits a formula) at each (row, col).
    fn typed_book(typed: &[((u32, u32), &str)]) -> (SheetPackage, crate::engine::Engine) {
        let mut pkg = new_xlsx();
        for r in 0..3 {
            pkg.workbook.sheets[0].set_cell(r, 0, Cell::number(r as f64 + 1.0));
        }
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        for &((r, c), src) in typed {
            eng.set_cell(&mut pkg.workbook, (0, r, c), Cell::formula(src));
        }
        (pkg, eng)
    }

    #[test]
    fn typed_xlookup_saves_with_xlfn_prefix() {
        // #776: a post-2007 function goes to the file with its prefix, or
        // Excel shows #NAME?.
        let (pkg, _) = typed_book(&[((0, 2), "XLOOKUP(2,A1:A3,A1:A3)")]);
        let (_, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="C1"><f>_xlfn.XLOOKUP(2,A1:A3,A1:A3)</f><v>2</v></c>"#),
            "{ws}"
        );
    }

    #[test]
    fn typed_spill_ref_saves_as_anchorarray() {
        let (pkg, _) = typed_book(&[((0, 2), "SEQUENCE(3)"), ((0, 3), "SUM(C1#)")]);
        let (_, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="D1"><f>SUM(_xlfn.ANCHORARRAY(C1))</f><v>6</v></c>"#),
            "{ws}"
        );
    }

    #[test]
    fn saved_file_formulas_reload_to_the_typed_display_and_value() {
        // #776 AC7: what the save adds, the load takes away again — the
        // reloaded formula shows as typed and recalculates to the same value.
        let srcs = [
            "LET(x,A1,x+1)",
            // Row 2: the implicit intersection picks A2.
            "@A1:A3",
            "LET(x,A1,x)+LET(x,A2,x*10)",
            "LAMBDA(a,b,a+b)(A1,A2)",
            "LET(x,1,LAMBDA(y,x+y)(2))",
            "LET(f,LAMBDA(x,x*3),f(A2))",
            "LAMBDA(x,[y],IF(ISOMITTED(y),x,x+y))(A3)",
            "LAMBDA(x,[y],IF(ISOMITTED(y),x,x+y))(A3,A1)",
            "SUM(E1#)",
            "XLOOKUP(2,A1:A3,A1:A3)*1.5",
            "LET(x,1.23456789E-12,x)*1000",
            // LET/LAMBDA names shared with a builtin called as one.
            "LET(sum,SUM(A1:A3),sum/SUM(A1:A2))",
            "LET(max,10,MAX(A1,max))",
            "LET(date,45306,DATE(YEAR(date),1,1))",
            "LAMBDA(text,LEN(TEXT(text,\"0.0\")))(A1)",
            // A LET name holding a lambda a call returned.
            "LET(mk,LAMBDA(n,LAMBDA(x,x+n)),inc,mk(A1),inc(5))",
        ];
        let mut typed: Vec<((u32, u32), &str)> = srcs
            .iter()
            .enumerate()
            .map(|(i, s)| ((i as u32, 2), *s))
            .collect();
        typed.push(((0, 4), "SEQUENCE(3)"));
        let (pkg, _) = typed_book(&typed);
        let before: Vec<CellValue> = (0..srcs.len() as u32)
            .map(|r| pkg.workbook.sheets[0].cell(r, 2).unwrap().value.clone())
            .collect();
        assert!(
            before.iter().all(|v| matches!(v, CellValue::Number(_))),
            "{before:?}"
        );

        let (mut re, _) = resaved(&pkg);
        for (r, src) in srcs.iter().enumerate() {
            let cell = re.workbook.sheets[0].cell(r as u32, 2).unwrap();
            let stored = cell.formula.as_deref().unwrap();
            assert_ne!(stored, *src, "saved without its file spelling");
            // Up to case: a parameter's call is spelled as the parameter.
            let shown = crate::formula::display_formula(stored);
            assert!(shown.eq_ignore_ascii_case(src), "{shown} for {src}");
        }
        // Recalculate from nothing, so no cached value can stand in.
        for r in 0..srcs.len() as u32 {
            let mut cell = re.workbook.sheets[0].cell(r, 2).unwrap().clone();
            cell.value = CellValue::Empty;
            re.workbook.sheets[0].set_cell(r, 2, cell);
        }
        rebuild(&mut re);
        for (r, src) in srcs.iter().enumerate() {
            assert_eq!(
                re.workbook.sheets[0].cell(r as u32, 2).unwrap().value,
                before[r],
                "{src}"
            );
        }
    }

    #[test]
    fn added_cf_and_dv_formulas_save_in_file_spelling() {
        // #776: rule formulas gridcore writes get their prefixes too.
        use crate::sheet::Dxf;
        let mut pkg = new_xlsx();
        let dxf = Dxf {
            fill: Some((255, 0, 0)),
            bold: None,
            ..Dxf::default()
        };
        pkg.add_conditional_format(
            0,
            (0, 0, 1, 0),
            "greaterThan",
            "XLOOKUP(1,B1:B2,C1:C2)",
            None,
            dxf,
        );
        pkg.add_data_validation(
            0,
            (0, 1, 1, 1),
            "whole",
            "between",
            "1",
            Some("MAXIFS(C1:C9,B1:B9,1)"),
        );
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains("<formula>_xlfn.XLOOKUP(1,B1:B2,C1:C2)</formula>"),
            "{ws}"
        );
        assert!(
            ws.contains("<formula1>1</formula1><formula2>_xlfn.MAXIFS(C1:C9,B1:B9,1)</formula2>"),
            "{ws}"
        );
    }

    #[test]
    fn typed_spill_saves_with_cm_and_new_metadata_part() {
        // #724 AC1: no metadata.xml yet — save creates it, with its
        // content-type override and workbook relationship.
        let (pkg, _) = typed_book(&[((0, 2), "SEQUENCE(3)")]);
        assert!(pkg.part("xl/metadata.xml").is_none());
        let (re, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="C1" cm="1"><f t="array" ref="C1:C3">_xlfn.SEQUENCE(3)</f>"#),
            "{ws}"
        );
        let meta = part_text(&re, "xl/metadata.xml");
        assert!(meta.contains(XLDAPR_TYPE), "{meta}");
        assert!(
            meta.contains(r#"<futureMetadata name="XLDAPR" count="1"><bk><extLst><ext uri="{bdbb8cdc-fa1e-496e-a857-3c3f30c029c3}"><xda:dynamicArrayProperties fDynamic="1" fCollapsed="0"/>"#),
            "{meta}"
        );
        assert!(
            meta.contains(r#"<cellMetadata count="1"><bk><rc t="1" v="0"/></bk></cellMetadata>"#),
            "{meta}"
        );
        assert!(meta.contains(&format!(r#"xmlns:xda="{DYNAMIC_ARRAY_NS}""#)));
        let ct = part_text(&re, "[Content_Types].xml");
        assert!(
            ct.contains(&format!(
                r#"<Override PartName="/xl/metadata.xml" ContentType="{SHEET_METADATA_CT}"/>"#
            )),
            "{ct}"
        );
        let rels = part_text(&re, "xl/_rels/workbook.xml.rels");
        assert!(
            rels.contains(&format!(
                r#"Type="{SHEET_METADATA_REL}" Target="metadata.xml""#
            )),
            "{rels}"
        );
        // Reloaded, it is Excel's dynamic array: `cm`, and it spills after a
        // fresh engine.
        let mut re = re;
        assert!(re.workbook.sheets[0].cell(0, 2).unwrap().has_cm());
        let mut eng = crate::engine::Engine::new(&re.workbook);
        eng.recalc_all(&mut re.workbook);
        assert_eq!(
            re.workbook.sheets[0].cell(0, 2).unwrap().spill,
            Some((3, 1))
        );
        // Saving again reuses the entry: nothing is appended twice.
        let (again, _) = resaved(&re);
        assert_eq!(part_text(&again, "xl/metadata.xml"), meta);
        assert_eq!(
            part_text(&again, "xl/_rels/workbook.xml.rels")
                .matches("sheetMetadata")
                .count(),
            1
        );
    }

    #[test]
    fn typed_one_by_one_array_result_saves_as_dynamic() {
        // #724 AC2: a 1x1 array result is still an array — `SEQUENCE(1)`, and a
        // FILTER that matches one row.
        let (pkg, _) = typed_book(&[((0, 1), "SEQUENCE(1)"), ((0, 2), "FILTER(A1:A3,A1:A3>2)")]);
        let (_, ws) = resaved(&pkg);
        assert!(
            ws.contains(
                r#"<c r="B1" cm="1"><f t="array" ref="B1">_xlfn.SEQUENCE(1)</f><v>1</v></c>"#
            ),
            "{ws}"
        );
        assert!(
            ws.contains(
                r#"<c r="C1" cm="1"><f t="array" ref="C1">_xlfn._xlws.FILTER(A1:A3,A1:A3&gt;2)</f><v>3</v></c>"#
            ),
            "{ws}"
        );
    }

    #[test]
    fn one_by_one_spill_anchor_survives_save_and_reload() {
        // #934 AC4+AC5: a 1x1 dynamic array saves exactly as before (an
        // anchor-only ref, never B1:B1) and reloads as a spill anchor, so
        // B1# still resolves after the round trip.
        let (pkg, _) = typed_book(&[((0, 1), "SEQUENCE(1)"), ((0, 2), "ROWS(B1#)")]);
        assert_eq!(val(&pkg, "B1"), CellValue::Number(1.0));
        assert_eq!(val(&pkg, "C1"), CellValue::Number(1.0));
        let (mut re, ws) = resaved(&pkg);
        assert!(
            ws.contains(
                r#"<c r="B1" cm="1"><f t="array" ref="B1">_xlfn.SEQUENCE(1)</f><v>1</v></c>"#
            ),
            "{ws}"
        );
        let mut eng = crate::engine::Engine::new(&re.workbook);
        eng.recalc_all(&mut re.workbook);
        assert_eq!(val(&re, "B1"), CellValue::Number(1.0));
        assert_eq!(val(&re, "C1"), CellValue::Number(1.0));
    }

    #[test]
    fn typed_maybe_array_formulas_save_as_dynamic_arrays() {
        // #777: a typed formula that could return an array saves as one even
        // if it never evaluated array-shaped — FILTER with no match yet
        // (#CALC!), INDIRECT/INDEX naming one cell — so neither Excel nor
        // xlsxy reopens it as a legacy implicit-intersection formula.
        let srcs = [
            "FILTER(A1:A3,A1:A3>5)",
            "INDIRECT(B1)",
            "IFERROR(FILTER(A1:A3,A1:A3>5),\"\")",
            "INDEX(A1:A3,2)",
        ];
        let mut typed: Vec<((u32, u32), &str)> = srcs
            .iter()
            .enumerate()
            .map(|(i, s)| ((i as u32, 2), *s))
            .collect();
        typed.insert(0, ((0, 1), "\"A2\""));
        let (pkg, _) = typed_book(&typed);
        let (re, ws) = resaved(&pkg);
        for (i, src) in srcs.iter().enumerate() {
            let f = format!(
                r#" cm="1"><f t="array" ref="C{n}">{}</f>"#,
                esc_text(&file_formula(src)),
                n = i + 1
            );
            let c = saved_cell(&ws, &format!("C{}", i + 1));
            assert!(c.contains(&f), "{f} in {c}");
            let cell = re.workbook.sheets[0].cell(i as u32, 2).unwrap();
            assert!(cell.is_array_formula() && cell.has_cm(), "{src}");
        }
        assert!(re.part("xl/metadata.xml").is_some());
    }

    #[test]
    fn typed_blocked_spill_saves_with_cm() {
        // #724 AC2: an anchor whose spill is blocked shows #SPILL! but is
        // still a dynamic array.
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(1, 2, Cell::text("x"));
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 0, 2), Cell::formula("SEQUENCE(3)"));
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 2).unwrap().value,
            CellValue::Error("#SPILL!".into())
        );
        let (_, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="C1" t="e" cm="1"><f t="array" ref="C1">_xlfn.SEQUENCE(3)</f>"#),
            "{ws}"
        );
    }

    #[test]
    fn typed_scalar_formula_saves_plain_without_metadata() {
        // #724 AC3: a formula that never produced an array is an ordinary
        // formula, as Excel writes it: no `cm`, no metadata part.
        let srcs = [
            "A1+1",
            "SUM(A1:A3)",
            "A1",
            "A1*A2",
            "XLOOKUP(2,A1:A3,A1:A3)",
            "IF(A1>1,1,2)",
            // One-cell ranges from functions are single values too.
            "OFFSET(A1,0,0)+1",
            "INDIRECT(\"A2\")*2",
            "-OFFSET(A1,1,0)",
        ];
        let typed: Vec<((u32, u32), &str)> = srcs
            .iter()
            .enumerate()
            .map(|(i, s)| ((i as u32, 2), *s))
            .collect();
        let (pkg, _) = typed_book(&typed);
        let (re, ws) = resaved(&pkg);
        for (i, src) in srcs.iter().enumerate() {
            let f = format!(
                r#"<c r="C{}"><f>{}</f>"#,
                i + 1,
                esc_text(&file_formula(src))
            );
            assert!(ws.contains(&f), "{f} in {ws}");
        }
        assert!(!ws.contains("cm="), "{ws}");
        assert!(re.part("xl/metadata.xml").is_none());
        assert!(!part_text(&re, "[Content_Types].xml").contains("metadata"));
    }

    #[test]
    fn retyped_scalar_over_dynamic_saves_plain() {
        // Typing over a dynamic array replaces the formula: a fresh Cell, so
        // the new scalar formula is not an array.
        let (mut pkg, mut eng) = typed_book(&[((0, 2), "SEQUENCE(3)")]);
        eng.set_cell(&mut pkg.workbook, (0, 0, 2), Cell::formula("A1+1"));
        let (re, ws) = resaved(&pkg);
        assert!(ws.contains(r#"<c r="C1"><f>A1+1</f><v>2</v></c>"#), "{ws}");
        assert!(!ws.contains("cm="), "{ws}");
        assert!(re.part("xl/metadata.xml").is_none());
    }

    #[test]
    fn typed_dynamic_mark_is_sticky() {
        // A FILTER that matched once stays a dynamic array when it later
        // matches nothing (#CALC!), like a loaded `cm`.
        let (mut pkg, mut eng) = typed_book(&[((0, 2), "FILTER(A1:A3,A1:A3>2)")]);
        eng.set_cell(&mut pkg.workbook, (0, 2, 0), Cell::number(0.0));
        let c1 = pkg.workbook.sheets[0].cell(0, 2).unwrap();
        assert_eq!(c1.value, CellValue::Error("#CALC!".into()));
        assert!(c1.is_dynamic());
        let (_, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="C1" t="e" cm="1"><f t="array" ref="C1">"#),
            "{ws}"
        );
    }

    #[test]
    fn typed_formula_extends_rich_only_metadata() {
        // #724 AC5: a metadata part with rich values and an XLDAPR type but no
        // XLDAPR futureMetadata or cellMetadata: the missing entries are
        // appended, every index in use keeps its meaning.
        let mut pkg = load_xlsx(&rich_error_fixture(RICH_ROWS)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 0, 3), Cell::formula("SEQUENCE(2)"));
        let (re, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="D1" cm="1"><f t="array" ref="D1:D2">_xlfn.SEQUENCE(2)</f>"#),
            "{ws}"
        );
        // The rich errors still carry their `vm` and decode.
        assert!(
            ws.contains(r#"<c r="A1" t="e" vm="1"><v>#VALUE!</v></c>"#),
            "{ws}"
        );
        let v = |r: u32| re.workbook.sheets[0].cell(r, 0).unwrap().value.clone();
        assert_eq!(v(0), CellValue::Error("#SPILL!".into()));
        assert_eq!(v(1), CellValue::Error("#CALC!".into()));
        assert_eq!(v(2), CellValue::Error("#GETTING_DATA".into()));
        assert!(re.workbook.sheets[0].cell(0, 3).unwrap().has_cm());
        // XLDAPR is type 1 already; its futureMetadata follows the rich one,
        // and cellMetadata comes before valueMetadata (schema order).
        let expected = RICH_METADATA.replace(
            "</futureMetadata><valueMetadata",
            &format!(
                r#"</futureMetadata><futureMetadata name="XLDAPR" count="1">{}</futureMetadata><cellMetadata count="1"><bk><rc t="1" v="0"/></bk></cellMetadata><valueMetadata"#,
                xldapr_dynamic_bk(&format!(r#" xmlns:xda="{DYNAMIC_ARRAY_NS}""#))
            ),
        );
        assert_eq!(part_text(&re, "xl/metadata.xml"), expected);
    }

    #[test]
    fn dynamic_cell_metadata_appends_only_what_is_missing() {
        const ROOT: &str =
            r#"<metadata xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">"#;
        let ns = format!(r#" xmlns:xda="{DYNAMIC_ARRAY_NS}""#);
        let bk = xldapr_dynamic_bk(&ns);
        // Rich values only: XLDAPR becomes type 2.
        let rich = format!(
            r#"{ROOT}<metadataTypes count="1"><metadataType name="XLRICHVALUE"/></metadataTypes><futureMetadata name="XLRICHVALUE" count="1"><bk/></futureMetadata><valueMetadata count="1"><bk><rc t="1" v="0"/></bk></valueMetadata></metadata>"#
        );
        assert_eq!(
            add_dynamic_cell_metadata(&rich),
            Some((
                Some(format!(
                    r#"{ROOT}<metadataTypes count="2"><metadataType name="XLRICHVALUE"/>{XLDAPR_TYPE}</metadataTypes><futureMetadata name="XLRICHVALUE" count="1"><bk/></futureMetadata><futureMetadata name="XLDAPR" count="1">{bk}</futureMetadata><cellMetadata count="1"><bk><rc t="2" v="0"/></bk></cellMetadata><valueMetadata count="1"><bk><rc t="1" v="0"/></bk></valueMetadata></metadata>"#
                )),
                1
            ))
        );
        // An XLDAPR block whose only entry is collapsed, and an unrelated
        // cellMetadata entry: a bk is appended to each.
        let collapsed = format!(
            r#"{ROOT}<metadataTypes count="1">{XLDAPR_TYPE}</metadataTypes><futureMetadata name="XLDAPR" count="1"><bk><extLst><ext uri="x"><xda:dynamicArrayProperties{ns} fDynamic="1" fCollapsed="1"/></ext></extLst></bk></futureMetadata><cellMetadata count="1"><bk><rc t="1" v="0"/></bk></cellMetadata></metadata>"#
        );
        let (out, index) = add_dynamic_cell_metadata(&collapsed).unwrap();
        assert_eq!(index, 2);
        let out = out.unwrap();
        assert!(
            out.contains(r#"<futureMetadata name="XLDAPR" count="2"><bk>"#),
            "{out}"
        );
        assert!(out.contains(&format!("{bk}</futureMetadata>")), "{out}");
        assert!(
            out.contains(r#"<cellMetadata count="2"><bk><rc t="1" v="0"/></bk><bk><rc t="1" v="1"/></bk></cellMetadata>"#),
            "{out}"
        );
        // Nothing yet at all (an empty root), and a self-closed cellMetadata.
        let empty = format!(r#"{ROOT}<cellMetadata count="0"/></metadata>"#);
        assert_eq!(
            add_dynamic_cell_metadata(&empty),
            Some((
                Some(format!(
                    r#"{ROOT}<metadataTypes count="1">{XLDAPR_TYPE}</metadataTypes><futureMetadata name="XLDAPR" count="1">{bk}</futureMetadata><cellMetadata count="1"><bk><rc t="1" v="0"/></bk></cellMetadata></metadata>"#
                )),
                1
            ))
        );
        // Not a part we understand: left alone.
        assert_eq!(
            add_dynamic_cell_metadata("<x:metadata xmlns:x=\"u\"></x:metadata>"),
            None
        );
        assert_eq!(add_dynamic_cell_metadata("<metadata>"), None);
        assert_eq!(add_dynamic_cell_metadata(""), None);
    }

    #[test]
    fn an_unreferenced_metadata_part_gets_its_relationship() {
        // #724 r1: the part is there but the workbook does not reference it
        // (no rel, no override): the `cm` only means something once it does.
        let mut pkg = load_xlsx(&cell_meta_fixture("")).unwrap();
        let rels = part_text(&pkg, "xl/_rels/workbook.xml.rels").replace(
            r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sheetMetadata" Target="metadata.xml"/>"#,
            "",
        );
        pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());
        let ct = part_text(&pkg, "[Content_Types].xml").replace(
            &format!(
                r#"<Override PartName="/xl/metadata.xml" ContentType="{SHEET_METADATA_CT}"/>"#
            ),
            "",
        );
        pkg.set_part("[Content_Types].xml", ct.into_bytes());
        let before = pkg.part("xl/metadata.xml").unwrap().to_vec();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 0, 0), Cell::formula("SEQUENCE(2)"));
        let (re, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="A1" cm="1"><f t="array" ref="A1:A2">"#),
            "{ws}"
        );
        assert_eq!(re.part("xl/metadata.xml").unwrap(), &before[..]);
        assert!(
            part_text(&re, "xl/_rels/workbook.xml.rels").contains(&format!(
                r#"Type="{SHEET_METADATA_REL}" Target="metadata.xml""#
            ))
        );
        assert!(part_text(&re, "[Content_Types].xml").contains(r#"PartName="/xl/metadata.xml""#));
    }

    #[test]
    fn a_new_metadata_part_sits_beside_a_workbook_stored_elsewhere() {
        // #724 r1: the workbook part is `wb/book.xml`; the new metadata part
        // and its relationship go with it, not to the conventional xl/ paths.
        let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#;
        let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let wb_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="sheets/one.xml"/></Relationships>"#;
        let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="wb/book.xml"/></Relationships>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/wb/book.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;
        let bytes = write_zip(&[
            ("[Content_Types].xml".into(), content_types.into()),
            ("_rels/.rels".into(), root_rels.into()),
            ("wb/book.xml".into(), workbook.into()),
            ("wb/_rels/book.xml.rels".into(), wb_rels.into()),
            ("wb/sheets/one.xml".into(), sheet.into()),
        ]);
        let mut pkg = load_xlsx(&bytes).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 0, 0), Cell::formula("SEQUENCE(2)"));
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let ws = part_text(&re, "wb/sheets/one.xml");
        assert!(
            ws.contains(r#"<c r="A1" cm="1"><f t="array" ref="A1:A2">"#),
            "{ws}"
        );
        assert!(re.part("wb/metadata.xml").is_some() && re.part("xl/metadata.xml").is_none());
        assert!(part_text(&re, "wb/_rels/book.xml.rels").contains(&format!(
            r#"Type="{SHEET_METADATA_REL}" Target="metadata.xml""#
        )));
        assert!(part_text(&re, "[Content_Types].xml").contains(r#"PartName="/wb/metadata.xml""#));
        assert!(re.workbook.sheets[0].cell(0, 0).unwrap().has_cm());
    }

    #[test]
    fn unreadable_metadata_part_is_left_alone() {
        // A metadata part we cannot extend: no `cm`, and the typed formula is
        // written as before (a plain `<f>` for a 1x1 result).
        let mut pkg = load_xlsx(&cell_meta_fixture("")).unwrap();
        pkg.set_part("xl/metadata.xml", b"<metadata>".to_vec());
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 0, 0), Cell::formula("SEQUENCE(1)"));
        let (re, ws) = resaved(&pkg);
        assert!(ws.contains(r#"<c r="A1"><f>_xlfn.SEQUENCE(1)</f>"#), "{ws}");
        assert_eq!(re.part("xl/metadata.xml").unwrap(), b"<metadata>");
    }

    /// A1:A3 = 1, 2, 3 and a legacy CSE array (no `cm`) anchored at C1 over
    /// `block`, with no stored values for the rest of the block.
    fn cse_book(block: &str, src: &str) -> SheetPackage {
        let rows = format!(
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="C1"><f t="array" ref="{block}">{src}</f><v>0</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row>"#
        );
        let mut pkg = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
        rebuild(&mut pkg);
        pkg
    }

    fn val(pkg: &SheetPackage, name: &str) -> CellValue {
        let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
        pkg.workbook.sheets[0]
            .cell(r, c)
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }

    #[test]
    fn cse_block_repeats_a_scalar_result() {
        let pkg = cse_book("C1:C3", "A1");
        for n in ["C1", "C2", "C3"] {
            assert_eq!(val(&pkg, n), CellValue::Number(1.0), "{n}");
        }
        let (re, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="C1"><f t="array" ref="C1:C3">A1</f><v>1</v></c>"#),
            "{ws}"
        );
        assert!(!ws.contains("cm="), "{ws}");
        assert!(!re.workbook.sheets[0].cell(0, 2).unwrap().has_cm());
    }

    #[test]
    fn cse_block_truncates_a_larger_result() {
        let pkg = cse_book("C1:C2", "A1:A3");
        assert_eq!(val(&pkg, "C2"), CellValue::Number(2.0));
        assert_eq!(val(&pkg, "C3"), CellValue::Empty);
    }

    #[test]
    fn cse_block_pads_a_smaller_result_with_na() {
        let pkg = cse_book("C1:D3", "A1:A2*10");
        assert_eq!(val(&pkg, "C2"), CellValue::Number(20.0));
        assert_eq!(val(&pkg, "D2"), CellValue::Number(20.0));
        assert_eq!(val(&pkg, "C3"), CellValue::Error("#N/A".into()));
        assert_eq!(val(&pkg, "D3"), CellValue::Error("#N/A".into()));
    }

    #[test]
    fn cse_block_refills_and_feeds_dependents_after_an_edit() {
        let mut pkg = cse_book("C1:C3", "A1");
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 4, 0), Cell::formula("C3+1"));
        assert_eq!(val(&pkg, "A5"), CellValue::Number(2.0));
        eng.set_cell(&mut pkg.workbook, (0, 0, 0), Cell::number(10.0));
        assert_eq!(val(&pkg, "C3"), CellValue::Number(10.0));
        assert_eq!(val(&pkg, "A5"), CellValue::Number(11.0));
    }

    #[test]
    fn a_value_typed_into_a_cse_block_is_refused() {
        // Excel refuses to change part of an array; the block owns its cells.
        let mut pkg = cse_book("C1:C3", "A1");
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        for typed in [99.0, 1.0] {
            eng.set_cell(&mut pkg.workbook, (0, 1, 2), Cell::number(typed));
            for n in ["C1", "C2", "C3"] {
                assert_eq!(val(&pkg, n), CellValue::Number(1.0), "{typed} {n}");
            }
        }
        // Still the block's: it follows its input.
        eng.set_cell(&mut pkg.workbook, (0, 0, 0), Cell::number(5.0));
        for n in ["C1", "C2", "C3"] {
            assert_eq!(val(&pkg, n), CellValue::Number(5.0), "{n}");
        }
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<f t="array" ref="C1:C3">A1</f><v>5</v>"#),
            "{ws}"
        );
    }

    #[test]
    fn a_refused_partial_edit_leaves_the_block_whole() {
        // Typing, pasting or clearing a plain cell inside the block changes
        // nothing: not the cell, not the anchor's extent, not the saved ref.
        // Also while a formula blocks the block, for its emptied cells.
        let mut pkg = cse_book("C1:C3", "A1");
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let before = pkg.workbook.sheets[0].clone();
        for (r, cell) in [
            (1, Cell::number(99.0)),
            (2, Cell::text("x")),
            (1, Cell::default()),
        ] {
            eng.set_cell(&mut pkg.workbook, (0, r, 2), cell);
            assert_eq!(pkg.workbook.sheets[0].cells, before.cells, "row {r}");
        }
        eng.set_cells(
            &mut pkg.workbook,
            0,
            vec![(1, 2, Cell::number(7.0)), (2, 2, Cell::number(8.0))],
        );
        assert_eq!(pkg.workbook.sheets[0].cells, before.cells);
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 2).unwrap().spill,
            Some((3, 1))
        );
        // Blocked by a formula in C2: C3 is emptied, and still the block's.
        // While the formula is there the block saves its anchor alone, as
        // Excel never writes a formula inside another cell's array.
        eng.set_cell(&mut pkg.workbook, (0, 1, 2), Cell::formula("7"));
        eng.set_cell(&mut pkg.workbook, (0, 2, 2), Cell::number(99.0));
        assert_eq!(val(&pkg, "C3"), CellValue::Empty);
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<f t="array" ref="C1">A1</f>"#), "{ws}");
        // Removing the formula is not refused, and frees the block.
        eng.set_cell(&mut pkg.workbook, (0, 1, 2), Cell::default());
        for n in ["C1", "C2", "C3"] {
            assert_eq!(val(&pkg, n), CellValue::Number(1.0), "{n}");
        }
    }

    #[test]
    fn a_formula_in_a_cse_block_blocks_it_until_it_goes() {
        let mut pkg = cse_book("C1:C3", "A1");
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 1, 2), Cell::formula("A2*100"));
        // The formula stays; the anchor keeps its own value (never `#SPILL!`)
        // and the rest of the block is cleared.
        assert_eq!(val(&pkg, "C2"), CellValue::Number(200.0));
        assert_eq!(val(&pkg, "C1"), CellValue::Number(1.0));
        assert_eq!(val(&pkg, "C3"), CellValue::Empty);
        // Blocked (no extent), it saves its anchor alone while the formula
        // is in its ref: a saved block over a formula is one Excel never
        // writes.
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<f t="array" ref="C1">A1</f>"#), "{ws}");
        eng.set_cell(&mut pkg.workbook, (0, 0, 0), Cell::number(5.0));
        assert_eq!(val(&pkg, "C1"), CellValue::Number(5.0));
        assert_eq!(val(&pkg, "C2"), CellValue::Number(200.0));
        assert_eq!(val(&pkg, "C3"), CellValue::Empty);
        eng.set_cell(&mut pkg.workbook, (0, 1, 2), Cell::default());
        for n in ["C1", "C2", "C3"] {
            assert_eq!(val(&pkg, n), CellValue::Number(5.0), "{n}");
        }
        // Refilled, it saves its whole ref again.
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<f t="array" ref="C1:C3">A1</f>"#), "{ws}");
    }

    #[test]
    fn a_formula_below_the_top_of_a_cse_block_clears_the_cells_above_it() {
        let mut pkg = cse_book("C1:C3", "A1");
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 2, 2), Cell::formula("7"));
        assert_eq!(val(&pkg, "C1"), CellValue::Number(1.0));
        assert_eq!(val(&pkg, "C2"), CellValue::Empty);
        assert_eq!(val(&pkg, "C3"), CellValue::Number(7.0));
    }

    #[test]
    fn a_cse_block_never_takes_cells_another_array_spilled_into() {
        let mut pkg = cse_book("C1:C3", "A1");
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        // B3 cannot spill into C3 while the block holds it.
        eng.set_cell(&mut pkg.workbook, (0, 2, 1), Cell::formula("SEQUENCE(1,2)"));
        assert_eq!(val(&pkg, "B3"), CellValue::Error("#SPILL!".into()));
        // A formula in C2 blocks the block and frees C3: B3 spills into it.
        eng.set_cell(&mut pkg.workbook, (0, 1, 2), Cell::formula("7"));
        assert_eq!(val(&pkg, "B3"), CellValue::Number(1.0));
        assert_eq!(val(&pkg, "C3"), CellValue::Number(2.0));
        // Clearing C2: C3 is B3's now, so the block stays blocked.
        eng.set_cell(&mut pkg.workbook, (0, 1, 2), Cell::default());
        assert_eq!(val(&pkg, "C1"), CellValue::Number(1.0));
        assert_eq!(val(&pkg, "C2"), CellValue::Empty);
        assert_eq!(val(&pkg, "C3"), CellValue::Number(2.0));
        assert_eq!(
            pkg.workbook.sheets[0].cell(2, 1).unwrap().spill,
            Some((1, 2))
        );
        assert_eq!(pkg.workbook.sheets[0].cell(0, 2).unwrap().spill, None);
        // Once B3 no longer spills, the block refills.
        eng.set_cell(&mut pkg.workbook, (0, 2, 1), Cell::default());
        for n in ["C1", "C2", "C3"] {
            assert_eq!(val(&pkg, n), CellValue::Number(1.0), "{n}");
        }
    }

    #[test]
    fn pasting_values_over_a_whole_cse_block_freezes_it() {
        // Anchor first, as a paste writes row-major: replacing the anchor
        // ends the block, so the values pasted after it are plain constants.
        let mut pkg = cse_book("C1:C3", "A1");
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        for r in 0..3 {
            eng.set_cell(&mut pkg.workbook, (0, r, 2), Cell::number(1.0));
        }
        eng.set_cell(&mut pkg.workbook, (0, 0, 0), Cell::number(5.0));
        for n in ["C1", "C2", "C3"] {
            assert_eq!(val(&pkg, n), CellValue::Number(1.0), "{n}");
        }
        let ws = saved_sheet1(&pkg);
        assert!(!ws.contains("t=\"array\""), "{ws}");
    }

    #[test]
    fn undoing_a_formula_typed_into_a_cse_block_refills_it() {
        // xlsxy's undo restores only the edited cell (`restore_cell`).
        let mut pkg = cse_book("C1:C3", "A1");
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let before = pkg.workbook.sheets[0].cell(1, 2).cloned().unwrap();
        eng.set_cell(&mut pkg.workbook, (0, 1, 2), Cell::formula("7"));
        assert_eq!(val(&pkg, "C3"), CellValue::Empty);
        eng.restore_cell(&mut pkg.workbook, (0, 1, 2), before);
        for n in ["C1", "C2", "C3"] {
            assert_eq!(val(&pkg, n), CellValue::Number(1.0), "{n}");
        }
        eng.set_cell(&mut pkg.workbook, (0, 0, 0), Cell::number(5.0));
        for n in ["C1", "C2", "C3"] {
            assert_eq!(val(&pkg, n), CellValue::Number(5.0), "{n}");
        }
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<f t="array" ref="C1:C3">A1</f>"#), "{ws}");
    }

    #[test]
    fn inserting_a_row_inside_a_cse_block_refills_the_grown_block() {
        let mut pkg = cse_book("C1:C3", "A1");
        crate::edit::insert_rows(&mut pkg.workbook, 0, 1, 1);
        rebuild(&mut pkg);
        for n in ["C1", "C2", "C3", "C4"] {
            assert_eq!(val(&pkg, n), CellValue::Number(1.0), "{n}");
        }
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 0, 0), Cell::number(5.0));
        for n in ["C1", "C2", "C3", "C4"] {
            assert_eq!(val(&pkg, n), CellValue::Number(5.0), "{n}");
        }
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<f t="array" ref="C1:C4">A1</f>"#), "{ws}");
    }

    #[test]
    fn one_cell_cse_block_truncates_and_keeps_its_ref() {
        let pkg = cse_book("C1", "A1:A3");
        assert_eq!(val(&pkg, "C1"), CellValue::Number(1.0));
        assert_eq!(val(&pkg, "C2"), CellValue::Empty);
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<f t="array" ref="C1">A1:A3</f>"#), "{ws}");
    }

    #[test]
    fn a_whole_sheet_array_ref_falls_back_to_its_anchor() {
        // #846 AC1 (r2-huge-ref): a crafted CSE `ref` over the whole grid is
        // cut to its anchor at load, before any fill could walk it.
        let rows =
            r#"<row r="1"><c r="A1"><f t="array" ref="A1:XFD1048576">1</f><v>1</v></c></row>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let a1 = pkg.workbook.sheets[0].cell(0, 0).unwrap();
        assert_eq!(a1.spill, None);
        assert_eq!(a1.f_attrs.as_deref(), Some(r#" t="array" ref="A1""#));
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        assert_eq!(pkg.workbook.sheets[0].cells.len(), 1);
        let (_, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="A1"><f t="array" ref="A1">1</f><v>1</v></c>"#),
            "{ws}"
        );
    }

    #[test]
    fn a_huge_dynamic_array_ref_falls_back_to_its_anchor() {
        // #846 AC2: the same for a dynamic array, whose loaded extent the
        // first recalc would otherwise clear cell by cell; it then spills
        // what it evaluates to.
        let rows = r#"<row r="1"><c r="A1" cm="1"><f t="array" ref="A1:XFD1048576">_xlfn.SEQUENCE(2)</f><v>1</v></c></row>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let a1 = pkg.workbook.sheets[0].cell(0, 0).unwrap();
        assert_eq!(a1.spill, None);
        assert_eq!(a1.f_attrs.as_deref(), Some(r#" t="array" ref="A1""#));
        assert!(a1.is_dynamic());
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 0).unwrap().spill,
            Some((2, 1))
        );
        let (_, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="A1" cm="1"><f t="array" ref="A1:A2">_xlfn.SEQUENCE(2)</f>"#),
            "{ws}"
        );
    }

    #[test]
    fn an_extent_from_an_xref_attribute_is_not_loaded() {
        // #846 r1 M1: the load's extent and the cap read a `ref` the same
        // way, so `xref="…"` names no block, with or without a real `ref`
        // after it.
        for f in [
            r#"<f t="array" xref="A1:XFD1048576">1</f>"#,
            r#"<f t="array" xref="A1:XFD1048576" ref="A1">1</f>"#,
        ] {
            let rows = format!(r#"<row r="1"><c r="A1">{f}<v>1</v></c></row>"#);
            let mut pkg = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
            let a1 = pkg.workbook.sheets[0].cell(0, 0).unwrap();
            assert!(
                a1.spill.is_none_or(|ext| ext == (1, 1)),
                "{f}: {:?}",
                a1.spill
            );
            let mut eng = crate::engine::Engine::new(&pkg.workbook);
            eng.recalc_all(&mut pkg.workbook);
            assert_eq!(pkg.workbook.sheets[0].cells.len(), 1, "{f}");
        }
    }

    #[test]
    fn the_cap_drops_an_extent_no_ref_backs() {
        // #846 r1 M1: whatever set a loaded extent, the cap keeps it only
        // where a `ref` from the cell names exactly that block.
        let mut sheet = Sheet::default();
        let mut a1 = Cell::formula("1");
        a1.f_attrs = Some(r#" t="array" ref="A1""#.into());
        a1.spill = Some((crate::sheet::MAX_ROWS, crate::sheet::MAX_COLS));
        sheet.cells.insert((0, 0), a1);
        let mut b1 = Cell::formula("1");
        b1.f_attrs = Some(r#" t="array" ref="B1:B2""#.into());
        b1.spill = Some((2, 1));
        sheet.cells.insert((0, 1), b1);
        cap_array_refs(&mut sheet);
        assert_eq!(sheet.cell(0, 0).unwrap().spill, None);
        assert_eq!(sheet.cell(0, 1).unwrap().spill, Some((2, 1)));
    }

    #[test]
    fn many_medium_array_refs_share_one_budget() {
        // #846 AC4: the cap is one budget per sheet, so refs that each fit
        // can't add up. 40 cells give 2 * 40 + 64Ki = 65 616 block cells:
        // six 100x100 refs fit, in key order, and the rest fall back.
        let rows: String = (0..40)
            .map(|i| {
                let n = 1 + 100 * i;
                format!(
                    r#"<row r="{n}"><c r="A{n}"><f t="array" ref="A{n}:CV{}">1</f><v>1</v></c></row>"#,
                    n + 99
                )
            })
            .collect();
        let pkg = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
        let kept: Vec<u32> = pkg.workbook.sheets[0]
            .cells
            .iter()
            .filter(|(_, cl)| cl.spill.is_some())
            .map(|(&(r, _), _)| r)
            .collect();
        assert_eq!(kept, vec![0, 100, 200, 300, 400, 500]);
        let a601 = pkg.workbook.sheets[0].cell(600, 0).unwrap();
        assert_eq!(a601.f_attrs.as_deref(), Some(r#" t="array" ref="A601""#));
        let a501 = pkg.workbook.sheets[0].cell(500, 0).unwrap();
        assert_eq!(
            a501.f_attrs.as_deref(),
            Some(r#" t="array" ref="A501:CV600""#)
        );
    }

    #[test]
    fn genuine_cse_and_spill_refs_load_unchanged() {
        // #846 AC3: blocks whose cells the file holds keep their `ref` and
        // extent, and the cap leaves an array `ref` that doesn't start at its
        // cell (never a block) as it is, whatever it names.
        let rows = r#"<row r="1"><c r="A1"><v>1</v></c><c r="D1"><f t="array" ref="D1:D3">A1:A3*2</f><v>2</v></c><c r="E1" cm="1"><f t="array" ref="E1:E3">A1:A3*3</f><v>3</v></c><c r="F1"><f t="array" ref="G1:XFD1048576">A1</f><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c><c r="D2"><v>4</v></c><c r="E2"><v>6</v></c></row><row r="3"><c r="A3"><v>3</v></c><c r="D3"><v>6</v></c><c r="E3"><v>9</v></c></row>"#;
        let pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let sheet = &pkg.workbook.sheets[0];
        let at = |c: u32| sheet.cell(0, c).unwrap();
        assert_eq!(at(3).spill, Some((3, 1)));
        assert_eq!(at(3).f_attrs.as_deref(), Some(r#" t="array" ref="D1:D3""#));
        assert_eq!(at(4).spill, Some((3, 1)));
        assert_eq!(at(4).f_attrs.as_deref(), Some(r#" t="array" ref="E1:E3""#));
        assert_eq!(at(5).spill, None);
        assert_eq!(
            at(5).f_attrs.as_deref(),
            Some(r#" t="array" ref="G1:XFD1048576""#)
        );
        let (_, ws) = resaved(&pkg);
        for f in [
            r#"<c r="D1"><f t="array" ref="D1:D3">A1:A3*2</f><v>2</v></c>"#,
            r#"<c r="E1" cm="1"><f t="array" ref="E1:E3">A1:A3*3</f><v>3</v></c>"#,
            // Save has always given a borrowed `ref` back to its own cell.
            r#"<c r="F1"><f t="array" ref="F1">A1</f><v>1</v></c>"#,
        ] {
            assert!(ws.contains(f), "{f}\n{ws}");
        }
    }

    #[test]
    fn legacy_cse_array_gets_no_cm_on_save() {
        // #724 AC6: a loaded Ctrl+Shift+Enter array (`t="array"`, no `cm`) is
        // not turned into a dynamic array.
        let rows = r#"<row r="1"><c r="A1"><v>1</v></c><c r="D1"><f t="array" ref="D1:D3">A1:A3*2</f><v>2</v></c></row><row r="2"><c r="A2"><v>2</v></c><c r="D2"><v>4</v></c></row><row r="3"><c r="A3"><v>3</v></c><c r="D3"><v>6</v></c></row>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        assert!(!pkg.workbook.sheets[0].cell(0, 3).unwrap().is_dynamic());
        let (_, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="D1"><f t="array" ref="D1:D3">A1:A3*2</f>"#),
            "{ws}"
        );
        assert!(!ws.contains("cm="), "{ws}");
    }

    /// A1:A3 = 1, 2, 3 and a plain loaded (legacy) `A1:A3*2` in B2, which
    /// reduces to one value (the engine's `@` fallback: the top-left, 2)
    /// instead of spilling.
    const LEGACY_ROWS: &str = r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c><c r="B2"><f>A1:A3*2</f><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row>"#;

    #[test]
    fn restyle_legacy_formula_stays_plain() {
        // Formatting is not typing: a loaded legacy formula restyled through
        // set_cell stays legacy — no spill, no `cm`, metadata untouched.
        let mut pkg = load_xlsx(&cell_meta_fixture(LEGACY_ROWS)).unwrap();
        let before = pkg.part("xl/metadata.xml").unwrap().to_vec();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let mut cell = pkg.workbook.sheets[0].cell(1, 1).cloned().unwrap();
        cell.style = 1;
        eng.set_cell(&mut pkg.workbook, (0, 1, 1), cell);
        let b2 = pkg.workbook.sheets[0].cell(1, 1).unwrap();
        assert_eq!(
            (b2.spill, b2.is_modern(), b2.is_dynamic()),
            (None, false, false)
        );
        let (re, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="B2" s="1"><f>A1:A3*2</f><v>2</v></c>"#),
            "{ws}"
        );
        assert!(!ws.contains("cm="), "{ws}");
        assert_eq!(re.part("xl/metadata.xml").unwrap(), &before[..]);
    }

    #[test]
    fn restyle_cse_keeps_f_attrs() {
        let rows = r#"<row r="1"><c r="A1"><v>1</v></c><c r="D1"><f t="array" ref="D1:D3">A1:A3*2</f><v>2</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        let mut cell = pkg.workbook.sheets[0].cell(0, 3).cloned().unwrap();
        cell.style = 1;
        eng.set_cell(&mut pkg.workbook, (0, 0, 3), cell);
        let d1 = pkg.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.f_attrs.as_deref(), Some(r#" t="array" ref="D1:D3""#));
        assert!(!d1.is_dynamic() && !d1.is_modern());
        let (_, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="D1" s="1"><f t="array" ref="D1:D3">"#),
            "{ws}"
        );
    }

    /// #785: the `<f>` attributes the file's formulas were preserved with — a
    /// shared group's master (a group whose master we can't parse stays a
    /// group; a parseable one is expanded at load) and a data table — survive
    /// every path that rewrites such a cell without changing its formula: a
    /// restyle (`Engine::set_styles`, #784), the same formula through
    /// `set_cell` (Enter on an unchanged formula, a paste of it in place; a
    /// restyled clone here), and undo's `restore_cell`. Each save is the
    /// unedited one with only the style changed.
    #[test]
    fn restyle_and_undo_keep_shared_master_and_data_table_f_attrs() {
        let rows = concat!(
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><f t="shared" ref="B1:B3" si="0">[1]Sheet1!A1*2</f><v>2</v></c><c r="G1"><f t="dataTable" ref="G1:G2" dt2D="0" dtr="0" r1="A1"/><v>1</v></c></row>"#,
            r#"<row r="2"><c r="A2"><v>2</v></c><c r="B2"><f t="shared" si="0"/><v>4</v></c><c r="G2"><v>1</v></c></row>"#,
            r#"<row r="3"><c r="A3"><v>3</v></c><c r="B3"><f t="shared" si="0"/><v>6</v></c></row>"#,
        );
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let (_, unedited) = resaved(&pkg);
        assert!(
            unedited.contains(r#"<c r="B1"><f t="shared""#),
            "{unedited}"
        );
        assert!(
            unedited.contains(r#"<c r="G1"><f t="dataTable""#),
            "{unedited}"
        );
        let restyled = unedited
            .replace(r#"<c r="B1">"#, r#"<c r="B1" s="1">"#)
            .replace(r#"<c r="G1">"#, r#"<c r="G1" s="1">"#);

        let before: Vec<Cell> = [1, 6]
            .map(|c| pkg.workbook.sheets[0].cell(0, c).cloned().unwrap())
            .into();
        for c in [1, 6] {
            let mut cell = pkg.workbook.sheets[0].cell(0, c).cloned().unwrap();
            cell.style = 1;
            eng.set_cell(&mut pkg.workbook, (0, 0, c), cell);
        }
        let (re, ws) = resaved(&pkg);
        assert_eq!(ws, restyled);
        let sheet = &re.workbook.sheets[0];
        for (r, c) in [(0, 1), (1, 1), (2, 1), (0, 6)] {
            let (a, b) = (
                sheet.cell(r, c).unwrap(),
                pkg.workbook.sheets[0].cell(r, c).unwrap(),
            );
            assert_eq!((&a.formula, &a.value), (&b.formula, &b.value));
        }

        for (c, cell) in [1, 6].into_iter().zip(before) {
            eng.restore_cell(&mut pkg.workbook, (0, 0, c), cell);
        }
        assert_eq!(resaved(&pkg).1, unedited);

        eng.set_styles(&mut pkg.workbook, 0, &[(0, 1, 1), (0, 6, 1)]);
        assert_eq!(resaved(&pkg).1, restyled);
        eng.set_styles(&mut pkg.workbook, 0, &[(0, 1, 0), (0, 6, 0)]);
        assert_eq!(resaved(&pkg).1, unedited);
    }

    #[test]
    fn restore_cell_keeps_a_legacy_formula_legacy() {
        // Undo puts the snapshot back with restore_cell: the loaded legacy
        // formula is not stamped as typed, so it neither spills nor saves
        // with a `cm`.
        let mut pkg = load_xlsx(&cell_meta_fixture(LEGACY_ROWS)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let before = pkg.workbook.sheets[0].cell(1, 1).cloned().unwrap();
        eng.set_cell(&mut pkg.workbook, (0, 1, 1), Cell::number(7.0));
        eng.restore_cell(&mut pkg.workbook, (0, 1, 1), before.clone());
        let b2 = pkg.workbook.sheets[0].cell(1, 1).unwrap();
        assert_eq!(b2, &before);
        assert_eq!((b2.value.clone(), b2.spill), (CellValue::Number(2.0), None));
        let (_, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="B2"><f>A1:A3*2</f><v>2</v></c>"#),
            "{ws}"
        );
        assert!(!ws.contains("cm="), "{ws}");
    }

    #[test]
    fn restore_cell_keeps_cse_f_attrs() {
        let rows = r#"<row r="1"><c r="A1"><v>1</v></c><c r="D1"><f t="array" ref="D1:D3">A1:A3*2</f><v>2</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        let before = pkg.workbook.sheets[0].cell(0, 3).cloned().unwrap();
        eng.set_cell(&mut pkg.workbook, (0, 0, 3), Cell::number(1.0));
        eng.restore_cell(&mut pkg.workbook, (0, 0, 3), before);
        let d1 = pkg.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.f_attrs.as_deref(), Some(r#" t="array" ref="D1:D3""#));
        assert_eq!(d1.spill, Some((3, 1)));
    }

    #[test]
    fn recommitting_an_unchanged_typed_formula_keeps_it_dynamic() {
        // The editor builds a fresh Cell on Enter even when the text did not
        // change: the formula stays what it was — a dynamic array here.
        let (mut pkg, mut eng) = typed_book(&[((0, 2), "SEQUENCE(3)")]);
        eng.set_cell(&mut pkg.workbook, (0, 0, 2), Cell::formula("SEQUENCE(3)"));
        let c1 = pkg.workbook.sheets[0].cell(0, 2).unwrap();
        assert!(c1.is_modern() && c1.is_dynamic());
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 2).unwrap().spill,
            Some((3, 1))
        );
    }

    #[test]
    fn pasting_a_clone_does_not_bring_the_sources_formula_kind() {
        // #724 r1: paste sends a clone of the source cell to set_cell at
        // another address. When the target already has the same text, the
        // source's `<f>` attributes (a `ref` naming the source) and `cm` must
        // not land on it.
        let rows = r#"<row r="1"><c r="A1"><v>1</v></c><c r="D1"><f t="array" ref="D1">$A$1*2</f><v>2</v></c><c r="E1" cm="1"><f t="array" ref="E1:E3">$A$1:$A$3*2</f><v>2</v></c><c r="G1"><f>$A$1:$A$3*2</f><v>2</v></c></row><row r="2"><c r="A2"><v>2</v></c><c r="E2"><v>4</v></c></row><row r="3"><c r="A3"><v>3</v></c><c r="E3"><v>6</v></c></row>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let sheet =
            |pkg: &SheetPackage, c: u32| pkg.workbook.sheets[0].cell(0, c).cloned().unwrap();
        // The single-cell CSE D1 pasted to F1 twice (the second time F1
        // already has the same text).
        let d1 = sheet(&pkg, 3);
        eng.set_cell(&mut pkg.workbook, (0, 0, 5), d1.clone());
        eng.set_cell(&mut pkg.workbook, (0, 0, 5), d1);
        assert_eq!(sheet(&pkg, 5).f_attrs, None);
        // The dynamic E1 pasted onto the legacy G1 with the same text: G1
        // stays a legacy formula.
        let e1 = sheet(&pkg, 4);
        eng.set_cell(&mut pkg.workbook, (0, 0, 6), e1);
        let g1 = sheet(&pkg, 6);
        assert_eq!(
            (
                g1.f_attrs.clone(),
                g1.is_dynamic(),
                g1.is_modern(),
                g1.spill
            ),
            (None, false, false, None)
        );
        let (_, ws) = resaved(&pkg);
        assert!(
            !ws.contains(r#"<c r="F1"><f t="array" ref="D1">"#)
                && !ws.contains(r#"<c r="F1"><f t="array""#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<c r="G1"><f>$A$1:$A$3*2</f><v>2</v></c>"#),
            "{ws}"
        );
    }

    #[test]
    fn recommitting_a_cse_array_keeps_it_cse() {
        // Enter on an unchanged CSE array (a fresh Cell, no `<f>` attributes)
        // does not turn it into a dynamic array.
        let rows = r#"<row r="1"><c r="A1"><v>1</v></c><c r="D1"><f t="array" ref="D1:D3">A1:A3*2</f><v>2</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 0, 3), Cell::formula("A1:A3*2"));
        let d1 = pkg.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.f_attrs.as_deref(), Some(r#" t="array" ref="D1:D3""#));
        assert!(!d1.is_dynamic());
        let (_, ws) = resaved(&pkg);
        assert!(!ws.contains("cm="), "{ws}");
    }

    #[test]
    fn recommitting_a_loaded_dynamic_array_keeps_it_dynamic() {
        // A fresh Cell with the same text as a loaded `cm` anchor: it stays a
        // dynamic array with the file's own `cm`.
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        let src = pkg.workbook.sheets[0]
            .cell(0, 3)
            .unwrap()
            .formula
            .clone()
            .unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 0, 3), Cell::formula(&src));
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 3).unwrap().spill,
            Some((5, 1))
        );
        let (_, ws) = resaved(&pkg);
        assert!(
            ws.contains(r#"<c r="D1" cm="1"><f t="array" ref="D1:D5">"#),
            "{ws}"
        );
    }

    #[test]
    fn undoing_an_overwrite_through_restore_cells_restores_cm() {
        // xlsxy and gridwasm undo by restore_cells-ing the group's `before`
        // clones back: the anchor comes back exactly, `t="array"` attributes
        // and `cm`.
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        let before = pkg.workbook.sheets[0].cell(0, 3).cloned().unwrap();
        eng.set_cell(&mut pkg.workbook, (0, 0, 3), Cell::number(1.0));
        assert!(!saved_sheet1(&pkg).contains("cm="));
        eng.restore_cells(&mut pkg.workbook, 0, &[(0, 3, before.clone())]);
        assert_eq!(pkg.workbook.sheets[0].cell(0, 3).unwrap(), &before);
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1" cm="1"><f t="array" ref="D1:D5">"#),
            "{ws}"
        );
    }

    #[test]
    fn a_typed_formula_drops_a_foreign_cm() {
        // #825 AC6: a formula typed here with a `cm` from elsewhere (another
        // workbook's index) keeps none of it: save resolves a `cm` in this
        // package's metadata part, which it creates.
        let (mut pkg, mut eng) = typed_book(&[]);
        let mut cell = Cell::formula("A1:A3*2");
        cell.meta = Some(Box::new(crate::sheet::CellMeta {
            cm: Some("7".into()),
            ..Default::default()
        }));
        eng.set_cell(&mut pkg.workbook, (0, 0, 2), cell);
        let (re, ws) = resaved(&pkg);
        assert!(!ws.contains(r#"cm="7""#), "{ws}");
        assert!(
            ws.contains(r#"<c r="C1" cm="1"><f t="array" ref="C1:C3">A1:A3*2</f>"#),
            "{ws}"
        );
        let meta = part_text(&re, "xl/metadata.xml");
        assert!(meta.contains(r#"<cellMetadata count="1">"#), "{meta}");
    }

    /// Sheet 1's D1:D5 in `pkg`, as a grid copy takes it.
    fn copied_d1_d5(pkg: &SheetPackage) -> Vec<Vec<Cell>> {
        (0..5)
            .map(|r| {
                vec![
                    pkg.workbook.sheets[0]
                        .cell(r, 3)
                        .cloned()
                        .unwrap_or_default(),
                ]
            })
            .collect()
    }

    #[test]
    fn a_pasted_anchor_with_a_foreign_cm_saves_a_resolved_cm() {
        // #825 AC7: Excel's dynamic array (`cm="1"`) copied from workbook A
        // and pasted at its own address in workbook B, which has no metadata
        // part. Restored as an array block, it must not carry A's index: save
        // creates B's part and the `cm` names its entry.
        let mut a = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        crate::engine::Engine::new(&a.workbook).recalc_all(&mut a.workbook);
        let block = copied_d1_d5(&a);
        assert_eq!(block[0][0].spill, Some((5, 1)));
        let mut b = new_xlsx();
        for (r, v) in [3.0, 9.0, 1.0, 7.0, 5.0].into_iter().enumerate() {
            b.workbook.sheets[0].set_cell(r as u32, 0, Cell::number(v));
        }
        assert!(b.part("xl/metadata.xml").is_none());
        let mut eng = crate::engine::Engine::new(&b.workbook);
        eng.paste_block(&mut b.workbook, 0, (0, 3), &block);
        let d1 = b.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.spill, Some((5, 1)));
        assert!(!d1.has_cm() && d1.is_dynamic());
        let (re, ws) = resaved(&b);
        assert!(
            ws.contains(r#"<c r="D1" cm="1"><f t="array" ref="D1:D5">"#),
            "{ws}"
        );
        let meta = part_text(&re, "xl/metadata.xml");
        assert!(
            meta.contains(r#"<cellMetadata count="1"><bk><rc t="1" v="0"/></bk></cellMetadata>"#),
            "{meta}"
        );
    }

    #[test]
    fn a_same_workbook_cm_paste_saves_the_same_cm() {
        // #825 AC6: clearing the `cm` costs nothing in its own workbook: the
        // paste back in place saves `cm="1"` again, and the metadata part is
        // left exactly as it was (no second entry).
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        let before = pkg.part("xl/metadata.xml").unwrap().to_vec();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let block = copied_d1_d5(&pkg);
        eng.paste_block(&mut pkg.workbook, 0, (0, 3), &block);
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 3).unwrap().spill,
            Some((5, 1))
        );
        let (re, ws) = resaved(&pkg);
        assert!(ws.contains(SORT_ANCHOR), "{ws}");
        assert_eq!(re.part("xl/metadata.xml").unwrap(), &before[..]);
    }

    #[test]
    fn autofill_copy_is_typed_and_drops_source_cm_vm() {
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        assert!(pkg.workbook.sheets[0].cell(0, 3).unwrap().meta.is_some());
        crate::edit::autofill(
            &mut pkg.workbook,
            0,
            &crate::edit::FillReq::new((0, 3, 0, 3), (0, 4)),
        );
        let copy = pkg.workbook.sheets[0].cell(0, 4).unwrap();
        assert!(copy.formula.is_some());
        // Typed there (#785): modern, and none of the source's `cm`/`vm`.
        let meta = copy.meta.as_deref().unwrap();
        assert!(meta.modern && meta.cm.is_none() && meta.vm.is_none());
    }

    /// The `<f …>` opening tag (`ref` dropped) and whether the cell has a
    /// `cm`, of cell `name` in a saved sheet.
    fn saved_f_kind(ws: &str, name: &str) -> (String, bool) {
        let at = ws
            .find(&format!("<c r=\"{name}\""))
            .unwrap_or_else(|| panic!("{name} in {ws}"));
        let c = &ws[at..at + ws[at..].find("</c>").unwrap()];
        let f = c
            .find("<f")
            .map_or("", |i| &c[i..i + c[i..].find('>').unwrap()]);
        let f = f.split(" ref=").next().unwrap().to_string();
        (f, c.contains(" cm="))
    }

    /// #785: an autofilled copy of a formula saves the same kind of `<f>` as a
    /// Fill Right of it (`fill_changes` + `Engine::set_cell`): both are typed
    /// at the destination (#724), after the engine has evaluated them — the
    /// suite rebuilds and recalcs after an autofill.
    #[test]
    fn autofill_and_fill_copy_a_formula_alike() {
        let cse_sum = r#"<c r="C1"><f t="array" ref="C1:C3">SUM(A1:A3)</f><v>6</v></c>"#;
        let cse_array = r#"<c r="C1"><f t="array" ref="C1:C3">A1:A3*2</f><v>2</v></c>"#;
        let legacy = r#"<c r="C1"><f>A1:A3*2</f><v>2</v></c>"#;
        let dynamic = r#"<c r="C1" cm="1"><f t="array" ref="C1:C3">_xlfn._xlws.SORT(A1:A3,,-1)</f><v>3</v></c>"#;
        for (src, want) in [
            (cse_sum, ("<f".to_string(), false)),
            (cse_array, ("<f t=\"array\"".to_string(), true)),
            (legacy, ("<f t=\"array\"".to_string(), true)),
            (dynamic, ("<f t=\"array\"".to_string(), true)),
        ] {
            // A1:A3 for the source, B1:B3 for the copies in D1 to read.
            let rows: String = (1..=3)
                .map(|r| {
                    let extra = if r == 1 { src } else { "" };
                    format!(r#"<row r="{r}"><c r="A{r}"><v>{r}</v></c><c r="B{r}"><v>5</v></c>{extra}</row>"#)
                })
                .collect();
            let mut auto = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
            crate::edit::autofill(
                &mut auto.workbook,
                0,
                &crate::edit::FillReq::new((0, 2, 0, 2), (0, 3)),
            );
            let mut eng = crate::engine::Engine::new(&auto.workbook);
            eng.recalc_all(&mut auto.workbook);

            let mut fill = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
            let mut eng = crate::engine::Engine::new(&fill.workbook);
            eng.recalc_all(&mut fill.workbook);
            for (r, c, cell) in crate::edit::fill_changes(
                &fill.workbook.sheets[0],
                (0, 2, 0, 3),
                crate::edit::FillDir::Right,
            ) {
                eng.set_cell(&mut fill.workbook, (0, r, c), cell);
            }

            let (a, f) = (saved_sheet1(&auto), saved_sheet1(&fill));
            assert_eq!(
                saved_f_kind(&a, "D1"),
                saved_f_kind(&f, "D1"),
                "{src}
{a}
{f}"
            );
            assert_eq!(
                saved_f_kind(&a, "D1"),
                want,
                "{src}
{a}"
            );
        }
    }

    #[test]
    fn fill_copy_of_a_cm_cell_saves_without_its_cm() {
        // #777: Ctrl+D/Ctrl+R (fill_changes) copies, like autofill's, carry
        // none of the source's `<c>` metadata: a copy of a 1x1 dynamic array
        // whose own formula is scalar saves as a plain formula, not as an
        // array because the source was one.
        let rows = r#"<row r="1"><c r="A1"><v>3</v></c><c r="B1"><v>4</v></c><c r="D1" cm="1"><f t="array" ref="D1">A1*2</f><v>6</v></c></row>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        fill_from_d1(&mut pkg, (0, 4), false);
        let e1 = pkg.workbook.sheets[0].cell(0, 4).unwrap();
        assert!(!e1.has_cm() && e1.is_modern() && !e1.is_dynamic());
        let ws = saved_sheet1(&pkg);
        assert_eq!(
            saved_cell(&ws, "E1"),
            r#"<c r="E1"><f>B1*2</f><v>8</v></c>"#
        );
        assert!(saved_cell(&ws, "D1").contains(r#"cm="1""#), "{ws}");
    }

    #[test]
    fn deleting_a_cell_of_a_frozen_dynamic_array_keeps_its_block() {
        // #777 r3: a loaded `cm` dynamic array the engine can't evaluate is
        // frozen. Deleting one of its cells is a no-op (Excel's too): it
        // saves whole, not as an anchor over plain constants that Excel's
        // recalc would block with #SPILL!.
        let rows = concat!(
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="E1" cm="1"><f t="array" ref="E1:E3">_xlfn.PIVOTBY(A1,4)</f><v>7</v></c></row>"#,
            r#"<row r="2"><c r="E2"><v>8</v></c></row>"#,
            r#"<row r="3"><c r="E3"><v>9</v></c></row>"#,
        );
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 1, 4), Cell::default());
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 4).unwrap().spill,
            Some((3, 1))
        );
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(
                r#"<c r="E1" cm="1"><f t="array" ref="E1:E3">_xlfn.PIVOTBY(A1,4)</f><v>7</v></c>"#
            ),
            "{ws}"
        );
        assert!(ws.contains(r#"<c r="E2"><v>8</v></c>"#), "{ws}");
    }

    #[test]
    fn delete_outside_a_frozen_arrays_shifted_ref_still_clears() {
        // #777 r4: a row delete shifts a frozen anchor's `ref` (E1:E3 →
        // E1:E2) but not its stale extent, so E3 — now the user's "x" — is
        // not part of the block: Delete clears it. E2 still is: a no-op.
        let rows = concat!(
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="E1" cm="1"><f t="array" ref="E1:E3">_xlfn.PIVOTBY(A1,4)</f><v>7</v></c></row>"#,
            r#"<row r="2"><c r="E2"><v>8</v></c></row>"#,
            r#"<row r="3"><c r="E3"><v>9</v></c></row>"#,
            r#"<row r="4"><c r="E4" t="inlineStr"><is><t>x</t></is></c></row>"#,
        );
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        crate::edit::delete_rows(&mut pkg.workbook, 0, 1, 1);
        let at = |pkg: &SheetPackage, r: u32| {
            pkg.workbook.sheets[0]
                .cell(r, 4)
                .map_or(CellValue::Empty, |cl| cl.value.clone())
        };
        assert_eq!(at(&pkg, 2), CellValue::Text("x".into()));
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        let before = pkg.workbook.sheets[0].cell(2, 4).cloned().unwrap();
        eng.set_cell(&mut pkg.workbook, (0, 2, 4), Cell::default());
        assert_eq!(at(&pkg, 2), CellValue::Empty);
        eng.restore_cell(&mut pkg.workbook, (0, 2, 4), before);
        assert_eq!(at(&pkg, 2), CellValue::Text("x".into()));
        eng.set_cell(&mut pkg.workbook, (0, 1, 4), Cell::default());
        assert_eq!(at(&pkg, 1), CellValue::Number(9.0));
    }

    #[test]
    fn a_sort_cutting_an_array_block_is_refused() {
        // #840 (r4-pre-structural-spill): a sort through a frozen dynamic
        // array, or a legacy CSE block, would scatter its block; it is
        // refused and leaves the rows, the extent and the saved `ref` alone.
        for (cm, f) in [(r#" cm="1""#, "_xlfn.PIVOTBY(A1,4)"), ("", "A1:A3*2")] {
            let rows = format!(
                concat!(
                    r#"<row r="1"><c r="A1"><v>2</v></c><c r="E1"{cm}><f t="array" ref="E1:E3">{f}</f><v>7</v></c></row>"#,
                    r#"<row r="2"><c r="A2"><v>1</v></c><c r="E2"><v>8</v></c></row>"#,
                    r#"<row r="3"><c r="A3"><v>4</v></c><c r="E3"><v>9</v></c></row>"#,
                    r#"<row r="4"><c r="A4"><v>3</v></c><c r="E4" t="inlineStr"><is><t>x</t></is></c></row>"#,
                ),
                cm = cm,
                f = f
            );
            let mut pkg = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
            let before = pkg.workbook.sheets[0].cells.clone();
            assert!(crate::edit::sort_cuts_spill(&pkg.workbook, 0, 0, 3), "{f}");
            let n = crate::edit::sort_rows(&mut pkg.workbook, 0, 0, 3, &[(0, true)]);
            assert_eq!(n, 0, "{f}");
            assert_eq!(pkg.workbook.sheets[0].cells, before, "{f}");
            let ws = saved_sheet1(&pkg);
            assert!(ws.contains(r#"<f t="array" ref="E1:E3">"#), "{f}: {ws}");
        }
    }

    #[test]
    fn content_typed_into_a_frozen_cse_block_shrinks_its_saved_ref() {
        // r7 M1: a legacy CSE block the engine can't evaluate still takes
        // content typed into it (#837/#840): the anchor drops its extent and
        // then saves covering its own cell alone, never a block over the
        // typed value. Untouched, it keeps its whole ref.
        let rows = concat!(
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="E1"><f t="array" ref="E1:E3">_xlfn.PIVOTBY(A1,4)</f><v>7</v></c></row>"#,
            r#"<row r="2"><c r="E2"><v>8</v></c></row>"#,
            r#"<row r="3"><c r="E3"><v>9</v></c></row>"#,
        );
        for evaluated in [false, true] {
            let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
            let mut eng = crate::engine::Engine::new(&pkg.workbook);
            if evaluated {
                eng.recalc_all(&mut pkg.workbook);
            }
            assert!(eng.is_frozen(&pkg.workbook, (0, 0, 4)));
            let ws = saved_sheet1(&pkg);
            assert!(
                ws.contains(r#"<f t="array" ref="E1:E3">"#),
                "{evaluated}: {ws}"
            );
            assert!(eng.set_cell(&mut pkg.workbook, (0, 1, 4), Cell::number(5.0)));
            let ws = saved_sheet1(&pkg);
            assert!(
                ws.contains(
                    r#"<c r="E1"><f t="array" ref="E1">_xlfn.PIVOTBY(A1,4)</f><v>7</v></c>"#
                ),
                "{evaluated}: {ws}"
            );
            assert!(
                ws.contains(r#"<c r="E2"><v>5</v></c>"#),
                "{evaluated}: {ws}"
            );
        }
    }

    #[test]
    fn an_evaluated_cse_block_is_still_an_array_to_sort() {
        // #840 r1 m1: a legacy CSE `SUM` over its block evaluates to one
        // value, which fills the block (#775), and save keeps its `ref`. A
        // sort through a 3-row block is refused; a 1-row block moves with its
        // row, `ref` and all.
        let cse = |r: &str| format!(r#"<f t="array" ref="{r}">SUM(B1:B3)</f><v>6</v>"#);
        let rows = format!(
            concat!(
                r#"<row r="1"><c r="A1"><v>9</v></c><c r="B1"><v>1</v></c><c r="D1">{}</c></row>"#,
                r#"<row r="2"><c r="A2"><v>1</v></c><c r="B2"><v>2</v></c></row>"#,
                r#"<row r="3"><c r="A3"><v>5</v></c><c r="B3"><v>3</v></c></row>"#,
            ),
            cse("D1:D3")
        );
        let mut pkg = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 3).unwrap().spill,
            Some((3, 1))
        );
        assert!(crate::edit::sort_cuts_spill(&pkg.workbook, 0, 0, 2));
        assert_eq!(
            crate::edit::sort_rows(&mut pkg.workbook, 0, 0, 2, &[(0, true)]),
            0
        );
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1"><f t="array" ref="D1:D3">"#),
            "{ws}"
        );

        let rows = rows.replace(&cse("D1:D3"), &cse("D1:F1"));
        let mut pkg = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 3).unwrap().spill,
            Some((1, 3))
        );
        assert_eq!(
            crate::edit::sort_rows(&mut pkg.workbook, 0, 0, 2, &[(0, true)]),
            3
        );
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D3"><f t="array" ref="D3:F3">"#),
            "{ws}"
        );
    }

    #[test]
    fn a_sorted_one_row_frozen_array_keeps_its_block() {
        // #840: a sort moves a one-row frozen array with its row, `ref` and
        // all, so its block is still its own there: Delete on a cached cell
        // is a no-op, and save writes the block at its new row.
        let rows = concat!(
            r#"<row r="1"><c r="A1"><v>9</v></c><c r="E1" cm="1"><f t="array" ref="E1:G1">_xlfn.PIVOTBY(A1,4)</f><v>7</v></c><c r="F1"><v>8</v></c><c r="G1"><v>9</v></c></row>"#,
            r#"<row r="2"><c r="A2"><v>1</v></c></row>"#,
            r#"<row r="3"><c r="A3"><v>5</v></c></row>"#,
        );
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        assert_eq!(
            crate::edit::sort_rows(&mut pkg.workbook, 0, 0, 2, &[(0, true)]),
            3
        );
        let e3 = pkg.workbook.sheets[0].cell(2, 4).unwrap();
        assert_eq!(e3.spill, Some((1, 3)));
        assert!(e3.f_attrs.as_deref().unwrap().contains(r#"ref="E3:G3""#));
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 2, 5), Cell::default());
        let at = |c: u32| pkg.workbook.sheets[0].cell(2, c).map(|cl| cl.value.clone());
        assert_eq!(at(5), Some(CellValue::Number(8.0)));
        assert_eq!(
            pkg.workbook.sheets[0].cell(2, 4).unwrap().spill,
            Some((1, 3))
        );
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<f t="array" ref="E3:G3">"#), "{ws}");
        assert!(ws.contains(r#"<c r="F3"><v>8</v></c>"#), "{ws}");
    }

    #[test]
    fn cm_is_written_only_on_an_array_formula() {
        // A data-table `<f>` is kept verbatim but is not a dynamic array.
        let rows = r#"<row r="1"><c r="A1"><v>1</v></c><c r="G1" cm="1"><f t="dataTable" ref="G1:G2" dt2D="0" dtr="0" r1="A1"/><v>1</v></c></row>"#;
        let pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<c r="G1"><f t="dataTable""#), "{ws}");
        assert!(!ws.contains("cm="), "{ws}");
    }

    #[test]
    fn vm_is_kept_only_while_the_value_is_unchanged() {
        // B1's cached 6 is current (A1=3); B2's cached 6 is stale (A2=9, so
        // 18). E1 is a rich value with no formula.
        let rows = r#"<row r="1"><c r="A1"><v>3</v></c><c r="B1" vm="1"><f>A1*2</f><v>6</v></c><c r="E1" t="e" vm="2"><v>#VALUE!</v></c></row><row r="2"><c r="A2"><v>9</v></c><c r="B2" vm="3"><f>A2*2</f><v>6</v></c></row>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="B1" vm="1"><f>A1*2</f><v>6</v></c>"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<c r="E1" t="e" vm="2"><v>#VALUE!</v></c>"#),
            "{ws}"
        );
        assert!(ws.contains(r#"<c r="B2"><f>A2*2</f><v>18</v></c>"#), "{ws}");
    }

    #[test]
    fn a_pasted_value_keeps_its_vm_and_a_pasted_formula_drops_it() {
        // #840 (plan-paste-vm): a value pasted elsewhere is the same value,
        // so its value metadata (E1, a rich value) comes along, as Excel
        // copies a picture in a cell. A formula pasted elsewhere is typed
        // there: B1's `vm` describes B1's result, not the copy's, so it goes
        // even though the copy evaluates to the same 6.
        let rows = r#"<row r="1"><c r="A1"><v>3</v></c><c r="B1" vm="1"><f>A1*2</f><v>6</v></c><c r="E1" t="e" vm="2"><v>#VALUE!</v></c></row>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let sheet = &pkg.workbook.sheets[0];
        let b1 = sheet.cell(0, 1).unwrap().clone();
        let e1 = sheet.cell(0, 4).unwrap().clone();
        eng.paste_block(&mut pkg.workbook, 0, (2, 1), &[vec![b1]]);
        eng.paste_block(&mut pkg.workbook, 0, (2, 5), &[vec![e1]]);
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<c r="B3"><f>A1*2</f><v>6</v></c>"#), "{ws}");
        assert!(
            ws.contains(r#"<c r="F3" t="e" vm="2"><v>#VALUE!</v></c>"#),
            "{ws}"
        );
        // The sources keep theirs.
        assert!(ws.contains(r#"<c r="B1" vm="1">"#), "{ws}");
    }

    #[test]
    fn blocked_spill_anchor_keeps_cm_and_vm() {
        // Excel's #SPILL! anchor: D3 blocks the spill and recalc agrees, so the
        // value `vm` describes is unchanged.
        let anchor = r#"<c r="D1" t="e" cm="1" vm="1"><f t="array" ref="D1">_xlfn._xlws.SORT(A1:A5,,-1)</f><v>#SPILL!</v></c>"#;
        let rows = sort_anchor_rows(5, anchor).replacen(
            r#"<c r="A3"><v>1</v></c>"#,
            r#"<c r="A3"><v>1</v></c><c r="D3" t="inlineStr"><is><t>x</t></is></c>"#,
            1,
        );
        let mut pkg = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1" t="e" cm="1" vm="1"><f t="array" ref="D1">"#),
            "{ws}"
        );
    }

    #[test]
    fn phonetic_flag_round_trips() {
        let rows = r#"<row r="1"><c r="B1" s="1" ph="1"/><c r="F1" s="1" t="inlineStr" ph="1"><is><t>x</t></is></c></row>"#;
        let pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<c r="B1" s="1" ph="1"/>"#), "{ws}");
        assert!(ws.contains(r#"<c r="F1" s="1" t="s" ph="1">"#), "{ws}");
    }

    #[test]
    fn cm_on_a_plain_formula_is_not_kept() {
        // `cm` marks a dynamic array; on a plain `<f>` it means nothing, and
        // keeping it would turn the formula into an array on save.
        let rows =
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1" cm="1"><f>A1+1</f><v>2</v></c></row>"#;
        let pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        assert!(pkg.workbook.sheets[0].cell(0, 1).unwrap().meta.is_none());
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(r#"<c r="B1"><f>A1+1</f><v>2</v></c>"#), "{ws}");
    }

    #[test]
    fn blocking_a_loaded_spill_writes_the_anchor_alone() {
        // D3 blocks the loaded D1:D5 spill: the anchor turns #SPILL! and its
        // cells are cleared, so the loaded ref is stale.
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        eng.set_cell(&mut pkg.workbook, (0, 2, 3), Cell::text("x"));
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1" t="e" cm="1"><f t="array" ref="D1">_xlfn._xlws.SORT(A1:A5,,-1)</f><v>#SPILL!</v></c>"#),
            "{ws}"
        );
    }

    /// `set_cell` a clone of D1 with a new style, as xlsxy formats a cell.
    fn format_d1(pkg: &mut SheetPackage) {
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let mut cell = pkg.workbook.sheets[0].cell(0, 3).cloned().unwrap();
        cell.style = 1;
        eng.set_cell(&mut pkg.workbook, (0, 0, 3), cell);
    }

    #[test]
    fn legacy_array_block_with_a_scalar_result_saves_its_ref() {
        // A CSE array over D1:D3 (no `cm`) whose result is 1x1: the engine
        // repeats it over the block, which saves with its whole ref.
        let anchor = r#"<c r="D1"><f t="array" ref="D1:D3">SUM(A1:A5*A1:A5)</f><v>165</v></c>"#;
        let rows = sort_anchor_rows(5, anchor)
            .replacen(
                r#"<c r="A2"><v>9</v></c>"#,
                r#"<c r="A2"><v>9</v></c><c r="D2"><v>165</v></c>"#,
                1,
            )
            .replacen(
                r#"<c r="A3"><v>1</v></c>"#,
                r#"<c r="A3"><v>1</v></c><c r="D3"><v>165</v></c>"#,
                1,
            );
        let mut pkg = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1"><f t="array" ref="D1:D3">SUM(A1:A5*A1:A5)</f><v>165</v></c>"#),
            "{ws}"
        );
    }

    /// A legacy CSE block over D1:D3 (no `cm`) with a 1x1 result, as Excel
    /// saves it: the result repeated over the block. A1:A3 = 1, 2, 3.
    const CSE_SUM_ROWS: &str = concat!(
        r#"<row r="1"><c r="A1"><v>1</v></c><c r="D1"><f t="array" ref="D1:D3">SUM(A1:A3)</f><v>6</v></c></row>"#,
        r#"<row r="2"><c r="A2"><v>2</v></c><c r="D2"><v>6</v></c></row>"#,
        r#"<row r="3"><c r="A3"><v>3</v></c><c r="D3"><v>6</v></c></row>"#,
    );

    /// #785, as ruled for #775: a legacy CSE block owns its whole `ref`, as in
    /// Excel. A plain value or a styled blank typed into part of it is
    /// refused and never shrinks its saved `ref`; an untouched block keeps
    /// its ref. A formula typed into it blocks it (the anchor keeps its own
    /// value), and while it is there the anchor saves covering its own cell
    /// alone, as Excel never writes a formula inside another cell's array.
    #[test]
    fn only_a_formula_typed_inside_a_cse_block_shrinks_its_saved_ref() {
        const BLOCK: &str = r#"<f t="array" ref="D1:D3">SUM(A1:A3)</f>"#;
        let edited = |at: (u32, u32), cell: Cell| {
            let mut pkg = load_xlsx(&cell_meta_fixture(CSE_SUM_ROWS)).unwrap();
            let mut eng = crate::engine::Engine::new(&pkg.workbook);
            eng.recalc_all(&mut pkg.workbook);
            eng.set_cell(&mut pkg.workbook, (0, at.0, at.1), cell);
            saved_sheet1(&pkg)
        };

        let ws = edited((1, 3), Cell::formula("A1+1"));
        assert!(
            ws.contains(r#"<c r="D1"><f t="array" ref="D1">SUM(A1:A3)</f><v>6</v></c>"#),
            "{ws}"
        );
        assert!(ws.contains(r#"<c r="D2"><f>A1+1</f><v>2</v></c>"#), "{ws}");

        let ws = edited((2, 3), Cell::number(5.0));
        assert!(
            ws.contains(r#"<c r="D1"><f t="array" ref="D1:D3">SUM(A1:A3)</f><v>6</v></c>"#),
            "{ws}"
        );
        assert!(ws.contains(r#"<c r="D3"><v>6</v></c>"#), "{ws}");

        let styled = Cell {
            style: 1,
            ..Cell::default()
        };
        let ws = edited((1, 3), styled);
        assert!(ws.contains(BLOCK), "{ws}");

        // Recalculated but untouched: the block stays.
        let mut pkg = load_xlsx(&cell_meta_fixture(CSE_SUM_ROWS)).unwrap();
        rebuild(&mut pkg);
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(BLOCK), "{ws}");

        // Without an engine, the block saves as it was loaded.
        let pkg = load_xlsx(&cell_meta_fixture(CSE_SUM_ROWS)).unwrap();
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1"><f t="array" ref="D1:D3">SUM(A1:A3)</f><v>6</v></c>"#),
            "{ws}"
        );
    }

    /// A legacy Ctrl+Shift+Enter array over D1:D3 (no `cm`): A1:A3 = 1, 2, 3
    /// and D1 = A1:A3*2, with the block's other values stored plainly.
    const CSE_ROWS: &str = concat!(
        r#"<row r="1"><c r="A1"><v>1</v></c><c r="D1"><f t="array" ref="D1:D3">A1:A3*2</f><v>2</v></c></row>"#,
        r#"<row r="2"><c r="A2"><v>2</v></c><c r="D2"><v>4</v></c></row>"#,
        r#"<row r="3"><c r="A3"><v>3</v></c><c r="D3"><v>6</v></c></row>"#,
    );

    /// Rebuild the engine from the workbook, as xlsxy does after a
    /// structural edit.
    fn rebuild(pkg: &mut SheetPackage) {
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
    }

    fn col_d(pkg: &SheetPackage, rows: std::ops::Range<u32>) -> Vec<CellValue> {
        rows.map(|r| pkg.workbook.sheets[0].cell(r, 3).unwrap().value.clone())
            .collect()
    }

    #[test]
    fn format_edited_cse_array_survives_an_engine_rebuild() {
        let mut pkg = load_xlsx(&cell_meta_fixture(CSE_ROWS)).unwrap();
        format_d1(&mut pkg);
        rebuild(&mut pkg);
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 3).unwrap().spill,
            Some((3, 1))
        );
        assert_eq!(
            col_d(&pkg, 0..3),
            vec![
                CellValue::Number(2.0),
                CellValue::Number(4.0),
                CellValue::Number(6.0)
            ]
        );
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1" s="1"><f t="array" ref="D1:D3">A1:A3*2</f>"#),
            "{ws}"
        );
    }

    #[test]
    fn undoing_an_overwrite_of_a_cse_array_keeps_it_an_array() {
        // xlsxy undoes by set_cell-ing the `before` clone back.
        let mut pkg = load_xlsx(&cell_meta_fixture(CSE_ROWS)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let before = pkg.workbook.sheets[0].cell(0, 3).cloned().unwrap();
        eng.set_cell(&mut pkg.workbook, (0, 0, 3), Cell::number(1.0));
        eng.restore_cell(&mut pkg.workbook, (0, 0, 3), before);
        rebuild(&mut pkg);
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 3).unwrap().spill,
            Some((3, 1))
        );
        assert_eq!(
            col_d(&pkg, 0..3),
            vec![
                CellValue::Number(2.0),
                CellValue::Number(4.0),
                CellValue::Number(6.0)
            ]
        );
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1"><f t="array" ref="D1:D3">A1:A3*2</f>"#),
            "{ws}"
        );
    }

    /// The saved `<c r="{name}" …>…</c>` element.
    fn saved_cell<'a>(ws: &'a str, name: &str) -> &'a str {
        let start = ws
            .find(&format!(r#"<c r="{name}""#))
            .unwrap_or_else(|| panic!("{ws}"));
        let end = start + ws[start..].find("</c>").unwrap() + "</c>".len();
        &ws[start..end]
    }

    #[test]
    fn relocated_cse_clone_that_cannot_spill_never_claims_its_source_block() {
        // A paste re-submits the clone, source `ref` and all, at F5. Here a
        // CSE block over D1:D3 with a 1x1 result. A clone at a new address is
        // typing (#724): none of its source's `<f>` attributes come along.
        let anchor = r#"<c r="D1"><f t="array" ref="D1:D3">SUM(A1:A5*A1:A5)</f><v>165</v></c>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, anchor))).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let clone = pkg.workbook.sheets[0].cell(0, 3).cloned().unwrap();
        eng.set_cell(&mut pkg.workbook, (0, 4, 5), clone);
        assert_eq!(pkg.workbook.sheets[0].cell(4, 5).unwrap().f_attrs, None);
        let ws = saved_sheet1(&pkg);
        let f5 = saved_cell(&ws, "F5");
        assert!(f5.contains("SUM(A1:A5*A1:A5)</f><v>165</v>"), "{f5}");
        assert!(!f5.contains("D1:D3"), "{f5}");
        // The source block keeps its own ref.
        assert!(
            ws.contains(r#"<c r="D1"><f t="array" ref="D1:D3">SUM(A1:A5*A1:A5)</f>"#),
            "{ws}"
        );
    }

    #[test]
    fn relocated_cse_clone_blocked_by_a_neighbour_writes_its_own_anchor_ref() {
        // Typed at F5 (#724), its array result makes it a dynamic array.
        // The same for a 3x1 result blocked by F6 (#SPILL!). The clone
        // carries the source's spill extent; the engine drops it (#777), so
        // F6 counts as foreign.
        let rows = CSE_ROWS.replace(
            r#"<c r="D3"><v>6</v></c></row>"#,
            r#"<c r="D3"><v>6</v></c></row><row r="6"><c r="F6" t="inlineStr"><is><t>x</t></is></c></row>"#,
        );
        let mut pkg = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let clone = pkg.workbook.sheets[0].cell(0, 3).cloned().unwrap();
        assert!(clone.spill.is_some());
        eng.set_cell(&mut pkg.workbook, (0, 4, 5), clone);
        let ws = saved_sheet1(&pkg);
        let f5 = saved_cell(&ws, "F5");
        assert!(
            f5.contains(r#"<f t="array" ref="F5">A1:A3*2</f><v>#SPILL!</v>"#),
            "{f5}"
        );
    }

    #[test]
    fn an_edit_that_misses_a_loaded_array_keeps_its_text_verbatim() {
        // An insert below everything it reads, and a rename of a sheet it
        // doesn't name, leave its loaded text (prefixes and all) alone.
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        rebuild(&mut pkg);
        crate::edit::insert_rows(&mut pkg.workbook, 0, 10, 1);
        crate::edit::rename_sheet(&mut pkg.workbook, 0, "Main");
        rebuild(&mut pkg);
        let d1 = pkg.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.formula.as_deref(), Some("_xlfn._xlws.SORT(A1:A5,,-1)"));
        let ws = saved_sheet1(&pkg);
        assert!(ws.contains(SORT_ANCHOR), "{ws}");
    }

    /// A CSE block over D1:D3 with a 1x1 result, which the engine repeats
    /// over the whole block (see `Engine::fill_cse`).
    const SUM_BLOCK: &str =
        r#"<c r="D1"><f t="array" ref="D1:D3">SUM(A1:A5*A1:A5)</f><v>165</v></c>"#;

    /// Fill Down/Right from D1 into `target`, through set_cell as xlsxy does.
    fn fill_from_d1(pkg: &mut SheetPackage, target: (u32, u32), down: bool) {
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        let sel = (target.0, target.1, target.0, target.1);
        for (r, c, cell) in crate::edit::fill_changes(
            &pkg.workbook.sheets[0],
            sel,
            if down {
                crate::edit::FillDir::Down
            } else {
                crate::edit::FillDir::Right
            },
        ) {
            eng.set_cell(&mut pkg.workbook, (0, r, c), cell);
        }
    }

    #[test]
    fn an_array_cell_moved_without_set_cell_is_written_covering_its_anchor() {
        // Moved as a sort moves cells (Sheet::set_cell, no engine): the ref
        // it carries names D1:D3, which the writer must not claim from D6.
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SUM_BLOCK))).unwrap();
        rebuild(&mut pkg);
        let sheet = &mut pkg.workbook.sheets[0];
        let mut cell = sheet.cells.remove(&(0, 3)).unwrap();
        // The block filled D1:D3; drop that extent so the writer has only the
        // stale stored ref to go on (as for a block that could not fill).
        assert_eq!(cell.spill, Some((3, 1)));
        cell.spill = None;
        sheet.set_cell(5, 3, cell);
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D6"><f t="array" ref="D6">SUM(A1:A5*A1:A5)</f><v>165</v></c>"#),
            "{ws}"
        );
    }

    #[test]
    fn a_row_delete_inside_a_cse_block_saves_the_shrunk_ref_before_any_recalc() {
        // The writer takes `ref` from the anchor's spill extent, so that
        // extent must follow the edit even when nothing recalculates first.
        let rows = format!(r#"{CSE_ROWS}<row r="4"><c r="D4"><v>99</v></c></row>"#);
        let mut pkg = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
        crate::edit::delete_rows(&mut pkg.workbook, 0, 1, 1);
        let ws = saved_sheet1(&pkg);
        let d1 = saved_cell(&ws, "D1");
        assert!(
            d1.contains(r#"<f t="array" ref="D1:D2">A1:A2*2</f>"#),
            "{d1}"
        );
        rebuild(&mut pkg);
        assert_eq!(
            col_d(&pkg, 0..3),
            [2.0, 6.0, 99.0].map(CellValue::Number).to_vec()
        );
    }

    #[test]
    fn a_filled_down_cse_clone_never_claims_a_block_after_a_row_delete() {
        // The filled copy is typed (#724): no source `ref` to shrink onto it.
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SUM_BLOCK))).unwrap();
        fill_from_d1(&mut pkg, (1, 3), true);
        assert_eq!(pkg.workbook.sheets[0].cell(1, 3).unwrap().f_attrs, None);
        crate::edit::delete_rows(&mut pkg.workbook, 0, 0, 1);
        rebuild(&mut pkg);
        let ws = saved_sheet1(&pkg);
        let d1 = saved_cell(&ws, "D1");
        assert!(d1.contains("SUM(A1:A5*A1:A5)</f>"), "{d1}");
        assert!(!d1.contains(":D"), "{d1}");
    }

    #[test]
    fn a_filled_right_cse_clone_never_claims_a_block_after_a_column_delete() {
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SUM_BLOCK))).unwrap();
        fill_from_d1(&mut pkg, (0, 4), false);
        crate::edit::delete_cols(&mut pkg.workbook, 0, 3, 1);
        rebuild(&mut pkg);
        let ws = saved_sheet1(&pkg);
        let d1 = saved_cell(&ws, "D1");
        assert!(d1.contains("SUM(B1:B5*B1:B5)</f>"), "{d1}");
        assert!(!d1.contains(":D"), "{d1}");
    }

    #[test]
    fn insert_row_above_a_loaded_cse_array_shifts_its_text_and_ref() {
        let mut pkg = load_xlsx(&cell_meta_fixture(CSE_ROWS)).unwrap();
        rebuild(&mut pkg);
        crate::edit::insert_rows(&mut pkg.workbook, 0, 0, 1);
        rebuild(&mut pkg);
        assert_eq!(
            col_d(&pkg, 1..4),
            vec![
                CellValue::Number(2.0),
                CellValue::Number(4.0),
                CellValue::Number(6.0)
            ]
        );
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D2"><f t="array" ref="D2:D4">A2:A4*2</f><v>2</v></c>"#),
            "{ws}"
        );
    }

    #[test]
    fn insert_row_above_a_format_edited_cse_array_shifts_its_text_and_ref() {
        let mut pkg = load_xlsx(&cell_meta_fixture(CSE_ROWS)).unwrap();
        format_d1(&mut pkg);
        crate::edit::insert_rows(&mut pkg.workbook, 0, 0, 1);
        rebuild(&mut pkg);
        assert_eq!(
            pkg.workbook.sheets[0].cell(1, 3).unwrap().spill,
            Some((3, 1))
        );
        assert_eq!(
            col_d(&pkg, 1..4),
            vec![
                CellValue::Number(2.0),
                CellValue::Number(4.0),
                CellValue::Number(6.0)
            ]
        );
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D2" s="1"><f t="array" ref="D2:D4">A2:A4*2</f><v>2</v></c>"#),
            "{ws}"
        );
    }

    #[test]
    fn insert_row_above_a_non_spilling_cse_block_shifts_its_stored_ref() {
        // A 1x1 result the engine repeats over the block; the ref inside
        // `f_attrs` itself must move with the insert.
        let anchor = r#"<c r="D1"><f t="array" ref="D1:D3">SUM(A1:A5*A1:A5)</f><v>165</v></c>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, anchor))).unwrap();
        crate::edit::insert_rows(&mut pkg.workbook, 0, 0, 1);
        rebuild(&mut pkg);
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D2"><f t="array" ref="D2:D4">SUM(A2:A6*A2:A6)</f><v>165</v></c>"#),
            "{ws}"
        );
    }

    #[test]
    fn insert_row_above_a_loaded_dynamic_array_shifts_its_text_and_ref() {
        // A `cm` array loaded with its `t="array"` `f_attrs` shifts the same
        // way. Its text is reprinted without the `_xlfn._xlws.` prefix, and
        // the save puts it back (#776).
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        rebuild(&mut pkg);
        crate::edit::insert_rows(&mut pkg.workbook, 0, 0, 1);
        rebuild(&mut pkg);
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(
                r#"<c r="D2" cm="1"><f t="array" ref="D2:D6">_xlfn._xlws.SORT(A2:A6,,-1)</f><v>9</v></c>"#
            ),
            "{ws}"
        );
    }

    #[test]
    fn format_edited_dynamic_anchor_still_spills_after_an_engine_rebuild() {
        // xlsxy rebuilds the engine (Engine::new + recalc_all) after a
        // structural edit. A format edit is not typing: the anchor keeps its
        // `t="array"` attributes and its `cm`.
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, SORT_ANCHOR))).unwrap();
        format_d1(&mut pkg);
        let d1 = pkg.workbook.sheets[0].cell(0, 3).unwrap();
        assert!(d1.f_attrs.as_deref().is_some_and(is_array_f) && d1.has_cm());
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 3).unwrap().spill,
            Some((5, 1))
        );
        assert_eq!(
            pkg.workbook.sheets[0].cell(4, 3).unwrap().value,
            CellValue::Number(1.0)
        );
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1" s="1" cm="1"><f t="array" ref="D1:D5">"#),
            "{ws}"
        );
    }

    #[test]
    fn format_edit_of_a_blocked_dynamic_anchor_keeps_it_an_array() {
        let anchor = r#"<c r="D1" t="e" cm="1" vm="1"><f t="array" ref="D1">_xlfn._xlws.SORT(A1:A5,,-1)</f><v>#SPILL!</v></c>"#;
        let rows = sort_anchor_rows(5, anchor).replacen(
            r#"<c r="A3"><v>1</v></c>"#,
            r#"<c r="A3"><v>1</v></c><c r="D3" t="inlineStr"><is><t>x</t></is></c>"#,
            1,
        );
        let mut pkg = load_xlsx(&cell_meta_fixture(&rows)).unwrap();
        format_d1(&mut pkg);
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(r#"<c r="D1" s="1" t="e" cm="1" vm="1"><f t="array" ref="D1">_xlfn._xlws.SORT(A1:A5,,-1)</f><v>#SPILL!</v></c>"#),
            "{ws}"
        );
    }

    #[test]
    fn format_edit_of_a_scalar_dynamic_anchor_keeps_it_an_array() {
        // A dynamic array with a 1x1 result (3²+9²+1²+7²+5² = 165): no spill.
        let anchor = r#"<c r="D1" cm="1"><f t="array" ref="D1">SUM(A1:A5*A1:A5)</f><v>165</v></c>"#;
        let mut pkg = load_xlsx(&cell_meta_fixture(&sort_anchor_rows(5, anchor))).unwrap();
        format_d1(&mut pkg);
        assert_eq!(pkg.workbook.sheets[0].cell(0, 3).unwrap().spill, None);
        let ws = saved_sheet1(&pkg);
        assert!(
            ws.contains(
                r#"<c r="D1" s="1" cm="1"><f t="array" ref="D1">SUM(A1:A5*A1:A5)</f><v>165</v></c>"#
            ),
            "{ws}"
        );
    }

    /// A minimal two-sheet workbook with a real pivot: Data!A1:C5 sourcing a
    /// row-field/data-field pivot on the second sheet (stale cached output).
    fn pivot_fixture() -> Vec<u8> {
        let sheet1 = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="str"><v>Region</v></c><c r="B1" t="str"><v>Product</v></c><c r="C1" t="str"><v>Sales</v></c></row><row r="2"><c r="A2" t="str"><v>East</v></c><c r="B2" t="str"><v>Pen</v></c><c r="C2"><v>10</v></c></row><row r="3"><c r="A3" t="str"><v>West</v></c><c r="B3" t="str"><v>Pad</v></c><c r="C3"><v>20</v></c></row><row r="4"><c r="A4" t="str"><v>East</v></c><c r="B4" t="str"><v>Ink</v></c><c r="C4"><v>30</v></c></row><row r="5"><c r="A5" t="str"><v>West</v></c><c r="B5" t="str"><v>Pen</v></c><c r="C5"><v>40</v></c></row></sheetData></worksheet>"#;
        let sheet2 = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="3"><c r="A3" t="str"><v>Region</v></c><c r="B3" t="str"><v>Sum of Sales</v></c></row><row r="4"><c r="A4" t="str"><v>East</v></c><c r="B4"><v>999</v></c></row><row r="5"><c r="A5" t="str"><v>Grand Total</v></c><c r="B5"><v>999</v></c></row></sheetData></worksheet>"#;
        let sheet2_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../pivotTables/pivotTable1.xml"/></Relationships>"#;
        let pivot_table = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="PivotTable1" cacheId="1" dataCaption="Values"><location ref="A3:B5" firstHeaderRow="1" firstDataRow="1" firstDataCol="1"/><pivotFields count="3"><pivotField axis="axisRow" showAll="0"><items count="3"><item x="0"/><item x="1"/><item t="default"/></items></pivotField><pivotField showAll="0"/><pivotField dataField="1" showAll="0"/></pivotFields><rowFields count="1"><field x="0"/></rowFields><dataFields count="1"><dataField name="Sum of Sales" fld="2" baseField="0" baseItem="0"/></dataFields></pivotTableDefinition>"#;
        let cache = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<pivotCacheDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="rId1"><cacheSource type="worksheet"><worksheetSource ref="A1:C5" sheet="Data"/></cacheSource><cacheFields count="3"><cacheField name="Region" numFmtId="0"/><cacheField name="Product" numFmtId="0"/><cacheField name="Sales" numFmtId="0"/></cacheFields></pivotCacheDefinition>"#;
        let styles = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf/></cellStyleXfs><cellXfs count="1"><xf numFmtId="0" fontId="0"/></cellXfs></styleSheet>"#;
        let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/><sheet name="Report" sheetId="2" r:id="rId2"/></sheets><pivotCaches><pivotCache cacheId="1" r:id="rId5"/></pivotCaches></workbook>"#;
        let wb_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="pivotCache/pivotCacheDefinition1.xml"/></Relationships>"#;
        let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;

        write_zip(&[
            ("[Content_Types].xml".into(), content_types.into()),
            ("_rels/.rels".into(), root_rels.into()),
            ("xl/workbook.xml".into(), workbook.into()),
            ("xl/_rels/workbook.xml.rels".into(), wb_rels.into()),
            ("xl/worksheets/sheet1.xml".into(), sheet1.into()),
            ("xl/worksheets/sheet2.xml".into(), sheet2.into()),
            (
                "xl/worksheets/_rels/sheet2.xml.rels".into(),
                sheet2_rels.into(),
            ),
            ("xl/pivotTables/pivotTable1.xml".into(), pivot_table.into()),
            (
                "xl/pivotCache/pivotCacheDefinition1.xml".into(),
                cache.into(),
            ),
            ("xl/styles.xml".into(), styles.into()),
        ])
    }

    #[test]
    fn pivot_loads_refreshes_and_round_trips() {
        use crate::pivot::{PivotSource, refresh_pivots};
        let mut pkg = load_xlsx(&pivot_fixture()).unwrap();
        // Parsed and wired to its cache.
        assert_eq!(pkg.workbook.pivots.len(), 1);
        let piv = &pkg.workbook.pivots[0];
        assert_eq!(piv.name, "PivotTable1");
        assert_eq!(piv.sheet, 1);
        assert_eq!(piv.fields, vec!["Region", "Product", "Sales"]);
        assert_eq!(piv.row_fields, vec![0]);
        assert_eq!(piv.data_fields.len(), 1);
        assert!(!piv.unsupported);
        assert_eq!(
            piv.source,
            PivotSource::Range {
                sheet: "Data".into(),
                rect: (0, 0, 4, 2)
            }
        );

        // Refresh replaces the stale cached output with real aggregates.
        let outcome = refresh_pivots(&mut pkg.workbook);
        assert_eq!((outcome.refreshed, outcome.skipped), (1, 0));
        let report = &pkg.workbook.sheets[1];
        let val = |name: &str| {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            report
                .cell(r, c)
                .map(|cl| cl.value.clone())
                .unwrap_or_default()
        };
        assert_eq!(val("A3"), CellValue::Text("Region".into()));
        assert_eq!(val("B3"), CellValue::Text("Sum of Sales".into()));
        assert_eq!(val("A4"), CellValue::Text("East".into()));
        assert_eq!(val("B4"), CellValue::Number(40.0));
        assert_eq!(val("A5"), CellValue::Text("West".into()));
        assert_eq!(val("B5"), CellValue::Number(60.0));
        assert_eq!(val("A6"), CellValue::Text("Grand Total".into()));
        assert_eq!(val("B6"), CellValue::Number(100.0));
        // The location grew by the West row: A3:B5 → A3:B6.
        assert_eq!(pkg.workbook.pivots[0].location, (2, 0, 5, 1));

        // Save: location ref patched, cache marked refreshOnLoad; second
        // save byte-identical (deterministic writer).
        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).unwrap();
        let part = |name: &str| {
            let b = &pkg2.parts.iter().find(|(n, _)| n == name).unwrap().1;
            String::from_utf8_lossy(b).into_owned()
        };
        assert!(part("xl/pivotTables/pivotTable1.xml").contains("ref=\"A3:B6\""));
        assert!(part("xl/pivotCache/pivotCacheDefinition1.xml").contains("refreshOnLoad=\"1\""));
        assert_eq!(save_xlsx(&pkg2), save_xlsx(&pkg2));
        // The reloaded pivot refreshes to the same values (idempotent).
        let mut pkg3 = pkg2;
        let outcome = refresh_pivots(&mut pkg3.workbook);
        assert_eq!(outcome.refreshed, 1);
        let (r, c) = crate::sheet::parse_cell_name("B6").unwrap();
        assert_eq!(
            pkg3.workbook.sheets[1].cell(r, c).unwrap().value,
            CellValue::Number(100.0)
        );

        // Source edit → refresh reflects it.
        let (r, c) = crate::sheet::parse_cell_name("C2").unwrap();
        pkg3.workbook.sheets[0].set_cell(r, c, crate::sheet::Cell::number(100.0));
        refresh_pivots(&mut pkg3.workbook);
        let (r, c) = crate::sheet::parse_cell_name("B4").unwrap();
        assert_eq!(
            pkg3.workbook.sheets[1].cell(r, c).unwrap().value,
            CellValue::Number(130.0)
        );
    }

    #[test]
    fn renaming_a_pivots_source_sheet_keeps_it_wired() {
        use crate::pivot::{PivotSource, refresh_pivots};
        let mut pkg = load_xlsx(&pivot_fixture()).unwrap();
        assert_eq!(
            pkg.workbook.pivots[0].source,
            PivotSource::Range {
                sheet: "Data".into(),
                rect: (0, 0, 4, 2)
            }
        );

        // Rename the pivot's SOURCE sheet (index 0, "Data"). Without
        // rewriting `PivotSource::Range { sheet }` this orphans the pivot:
        // `refresh_pivots` looks the name up and just skips silently.
        crate::edit::rename_sheet(&mut pkg.workbook, 0, "Numbers");
        assert_eq!(
            pkg.workbook.pivots[0].source,
            PivotSource::Range {
                sheet: "Numbers".into(),
                rect: (0, 0, 4, 2)
            },
            "pivot source must follow the rename, case-insensitively matched"
        );

        let outcome = refresh_pivots(&mut pkg.workbook);
        assert_eq!(
            (outcome.refreshed, outcome.skipped),
            (1, 0),
            "renamed source must still resolve — no silent skip"
        );
        let val = |report: &Sheet, name: &str| {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            report
                .cell(r, c)
                .map(|cl| cl.value.clone())
                .unwrap_or_default()
        };
        assert_eq!(val(&pkg.workbook.sheets[1], "B4"), CellValue::Number(40.0));

        // A further edit on the (renamed) source sheet is picked up on the
        // next refresh — proof the wiring isn't just a one-shot coincidence.
        let (r, c) = crate::sheet::parse_cell_name("C2").unwrap();
        pkg.workbook.sheets[0].set_cell(r, c, crate::sheet::Cell::number(100.0));
        let outcome2 = refresh_pivots(&mut pkg.workbook);
        assert_eq!((outcome2.refreshed, outcome2.skipped), (1, 0));
        assert_eq!(val(&pkg.workbook.sheets[1], "B4"), CellValue::Number(130.0));

        // Save/load: the renamed source name round-trips through the cache
        // part and refresh still resolves it after reload.
        let bytes = save_xlsx(&pkg);
        let mut pkg2 = load_xlsx(&bytes).unwrap();
        assert_eq!(
            pkg2.workbook.pivots[0].source,
            PivotSource::Range {
                sheet: "Numbers".into(),
                rect: (0, 0, 4, 2)
            },
            "renamed source sheet lost on reload"
        );
        let outcome3 = refresh_pivots(&mut pkg2.workbook);
        assert_eq!((outcome3.refreshed, outcome3.skipped), (1, 0));
        assert_eq!(
            val(&pkg2.workbook.sheets[1], "B4"),
            CellValue::Number(130.0)
        );
    }

    #[test]
    fn edited_pivot_round_trips_through_save() {
        use crate::frame::Agg;
        use crate::pivot::{DataField, refresh_pivots};
        let mut pkg = load_xlsx(&pivot_fixture()).unwrap();
        // Simulate the TUI editor: rows = Product, value = Average of Sales.
        {
            let piv = &mut pkg.workbook.pivots[0];
            piv.row_fields = vec![1];
            piv.data_fields = vec![DataField {
                name: "Average of Sales".into(),
                field: 2,
                agg: Agg::Average,
            }];
            piv.edited = true;
        }
        refresh_pivots(&mut pkg.workbook);
        let bytes = save_xlsx(&pkg);

        // The rewritten definition survives a reload and refreshes to the
        // same result.
        let mut pkg2 = load_xlsx(&bytes).unwrap();
        let piv = &pkg2.workbook.pivots[0];
        assert_eq!(piv.row_fields, vec![1]);
        assert_eq!(piv.data_fields[0].agg, Agg::Average);
        assert_eq!(piv.data_fields[0].name, "Average of Sales");
        assert!(!piv.unsupported);
        refresh_pivots(&mut pkg2.workbook);
        let report = &pkg2.workbook.sheets[1];
        let val = |name: &str| {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            report
                .cell(r, c)
                .map(|cl| cl.value.clone())
                .unwrap_or_default()
        };
        // Products sorted: Ink 30, Pad 20, Pen (10+40)/2 = 25.
        assert_eq!(val("A3"), CellValue::Text("Product".into()));
        assert_eq!(val("A4"), CellValue::Text("Ink".into()));
        assert_eq!(val("B4"), CellValue::Number(30.0));
        assert_eq!(val("B5"), CellValue::Number(20.0));
        assert_eq!(val("A6"), CellValue::Text("Pen".into()));
        assert_eq!(val("B6"), CellValue::Number(25.0));
        // Grand total of an Average is the average over all records.
        assert_eq!(val("B7"), CellValue::Number(25.0));
        // Second save stays deterministic.
        let again = save_xlsx(&pkg2);
        assert_eq!(again, save_xlsx(&pkg2));
    }

    #[test]
    fn filtered_pivot_is_skipped_not_wrong() {
        // A pivot with a hidden item (an active filter) must keep its cached
        // cells rather than refresh to numbers that ignore the filter.
        let bytes = pivot_fixture();
        let s = String::from_utf8(bytes.clone()).ok(); // zip is binary; patch at part level instead
        drop(s);
        let mut pkg = load_xlsx(&bytes).unwrap();
        // Simulate: mark the loaded pivot as filtered the way the parser
        // does for h="1" items.
        pkg.workbook.pivots[0].unsupported = true;
        let outcome = crate::pivot::refresh_pivots(&mut pkg.workbook);
        assert_eq!((outcome.refreshed, outcome.skipped), (0, 1));
        let (r, c) = crate::sheet::parse_cell_name("B4").unwrap();
        assert_eq!(
            pkg.workbook.sheets[1].cell(r, c).unwrap().value,
            CellValue::Number(999.0) // stale cache, untouched
        );
    }

    #[test]
    fn created_pivot_round_trips_and_refreshes() {
        use crate::frame::Agg;
        use crate::pivot::{DataField, PivotSource, refresh_pivots};
        let mut pkg = new_xlsx();
        {
            let sh = &mut pkg.workbook.sheets[0];
            for (c, h) in ["Region", "Sales"].iter().enumerate() {
                sh.set_cell(0, c as u32, crate::sheet::Cell::text(h));
            }
            for (i, (r, v)) in [("East", 10.0), ("West", 20.0), ("East", 30.0)]
                .iter()
                .enumerate()
            {
                sh.set_cell(i as u32 + 1, 0, crate::sheet::Cell::text(r));
                sh.set_cell(i as u32 + 1, 1, crate::sheet::Cell::number(*v));
            }
        }
        let dest = pkg.add_sheet("Report");
        let idx = pkg
            .add_pivot(
                PivotSource::Range {
                    sheet: "Sheet1".into(),
                    rect: (0, 0, 3, 1),
                },
                vec!["Region".into(), "Sales".into()],
                DataField {
                    name: "Sum of Sales".into(),
                    field: 1,
                    agg: Agg::Sum,
                },
                dest,
                (2, 0), // A3, Excel's convention
            )
            .unwrap();
        // Configure like the editor would, then refresh.
        pkg.workbook.pivots[idx].row_fields = vec![0];
        let outcome = refresh_pivots(&mut pkg.workbook);
        assert_eq!(outcome.refreshed, 1);
        let val = |pkg: &SheetPackage, r: u32, c: u32| {
            pkg.workbook.sheets[dest]
                .cell(r, c)
                .map(|cl| cl.value.clone())
                .unwrap_or_default()
        };
        assert_eq!(val(&pkg, 3, 1), CellValue::Number(40.0)); // East
        assert_eq!(val(&pkg, 4, 1), CellValue::Number(20.0)); // West
        assert_eq!(val(&pkg, 5, 1), CellValue::Number(60.0)); // Grand

        // Save → reload: the created parts parse back into a supported,
        // fully-wired pivot that refreshes to the same values.
        let bytes = save_xlsx(&pkg);
        let mut pkg2 = load_xlsx(&bytes).unwrap();
        assert_eq!(pkg2.workbook.pivots.len(), 1);
        let piv = &pkg2.workbook.pivots[0];
        assert!(!piv.unsupported);
        assert_eq!(piv.row_fields, vec![0]);
        assert_eq!(piv.fields, vec!["Region", "Sales"]);
        assert_eq!(piv.sheet, 1);
        let outcome = refresh_pivots(&mut pkg2.workbook);
        assert_eq!(outcome.refreshed, 1);
        assert_eq!(
            pkg2.workbook.sheets[1].cell(5, 1).unwrap().value,
            CellValue::Number(60.0)
        );
        // Deterministic writer still holds with the new parts.
        assert_eq!(save_xlsx(&pkg2), save_xlsx(&pkg2));
        // Creating a second pivot picks fresh part names and cacheId.
        let idx2 = pkg2
            .add_pivot(
                PivotSource::Range {
                    sheet: "Sheet1".into(),
                    rect: (0, 0, 3, 1),
                },
                vec!["Region".into(), "Sales".into()],
                DataField {
                    name: "Count of Sales".into(),
                    field: 1,
                    agg: Agg::Count,
                },
                0,
                (5, 4),
            )
            .unwrap();
        assert_eq!(
            pkg2.workbook.pivots[idx2].part,
            "xl/pivotTables/pivotTable2.xml"
        );
        let wb_xml = String::from_utf8_lossy(pkg2.part("xl/workbook.xml").unwrap()).into_owned();
        assert!(wb_xml.contains("cacheId=\"2\""));
    }

    #[test]
    fn chart_strings_survive_a_round_trip_with_xml_entities() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource, DrawingKind};
        // Chart strings used to be stored as the RAW source slice, which was
        // harmless while the part round-tripped verbatim. Now an edited chart is
        // regenerated through `esc_attr`, so an undecoded `&amp;` would gain a
        // level of escaping per save — and a sheet name inside a `<c:f>` would
        // stop naming a real sheet.
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].name = "R&D".into();
        let src = |range| ChartSource {
            sheet: "R&D".into(),
            range,
            cat_col: 0,
        };
        let data = ChartData {
            title: "R&D spend <2026>".into(),
            kind: "column".into(),
            categories: vec!["Q1".into()],
            series: vec![ChartSeries {
                name: "Q&A".into(),
                values: vec![1.0],
                col: Some(1),
                values_ref: Some(src((1, 1, 1, 1))),
                name_ref: Some("'R&D'!$B$1".into()),
                ..Default::default()
            }],
            source: Some(src((0, 0, 1, 1))),
            categories_ref: Some(src((1, 0, 1, 0))),
            ..Default::default()
        };
        pkg.add_chart(0, (5, 0), (20, 8), &data);

        let chart = |p: &SheetPackage| match &p.workbook.sheets[0]
            .drawings
            .first()
            .expect("chart drawing")
            .kind
        {
            DrawingKind::Chart(c) => c.clone(),
            other => panic!("expected a chart drawing, got {other:?}"),
        };
        // One save/load is the fixed point: nothing gains an `amp;`, and the
        // refs still name the sheet they came from.
        let mut re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let got = chart(&re);
        assert_eq!(got.title, "R&D spend <2026>");
        assert_eq!(got.series[0].name, "Q&A");
        assert_eq!(got.series[0].name_ref.as_deref(), Some("'R&D'!$B$1"));
        assert_eq!(got.source.as_ref().map(|s| s.sheet.as_str()), Some("R&D"));

        // And again, through the regeneration path an edit takes.
        let mut cd = got;
        cd.edited = true;
        if let DrawingKind::Chart(slot) = &mut re.workbook.sheets[0].drawings[0].kind {
            *slot = cd;
        }
        let again = chart(&load_xlsx(&save_xlsx(&re)).unwrap());
        assert_eq!(again.title, "R&D spend <2026>");
        assert_eq!(again.series[0].name, "Q&A");
        assert_eq!(again.series[0].name_ref.as_deref(), Some("'R&D'!$B$1"));
        assert_eq!(
            again.series[0].values_ref.as_ref().map(|s| s.sheet.clone()),
            Some("R&D".to_string())
        );
    }

    #[test]
    fn an_edited_scatter_chart_is_kept_verbatim_rather_than_flattened() {
        use crate::sheet::DrawingKind;
        // `chart_space_xml` can only author bar/column/line/pie. A scatter chart
        // regenerated through it becomes a clustered COLUMN chart — and an EMPTY
        // one: `parse_chart` reads a scatter's `<c:xVal>`/`<c:yVal>` REFS (the
        // box, and `ChartSeries::point_refs`) but caches no numbers from them, so
        // every one of its series reaches the writer with nothing in any of the
        // three slots `<c:val>` comes from. Round-tripping the part beats
        // destroying it. (Converting such a chart deliberately is a different
        // door, and it re-derives rather than relabels — see the panel's
        // `chart_reauthored`.)
        assert!(!chart_kind_is_writable("scatter"));
        assert!(!chart_kind_is_writable("doughnut"));
        assert!(chart_kind_is_writable("column"));

        let mut pkg = new_xlsx();
        pkg.add_chart(
            0,
            (1, 1),
            (10, 6),
            &crate::sheet::ChartData {
                title: "Placeholder".into(),
                kind: "column".into(),
                ..Default::default()
            },
        );
        // Stand in for what Excel writes: a scatter plot with xVal/yVal.
        let scatter = "<?xml version=\"1.0\"?>\n<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\">\
<c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Scatter</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea><c:layout/>\
<c:scatterChart><c:scatterStyle val=\"lineMarker\"/><c:ser><c:idx val=\"0\"/><c:tx><c:v>S</c:v></c:tx>\
<c:xVal><c:numRef><c:f>Sheet1!$A$2:$A$3</c:f></c:numRef></c:xVal>\
<c:yVal><c:numRef><c:f>Sheet1!$B$2:$B$3</c:f></c:numRef></c:yVal></c:ser></c:scatterChart>\
</c:plotArea></c:chart></c:chartSpace>";
        if let Some(p) = pkg
            .parts
            .iter_mut()
            .find(|(n, _)| n == "xl/charts/chart1.xml")
        {
            p.1 = scatter.as_bytes().to_vec();
        }
        let mut re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        if let DrawingKind::Chart(cd) = &mut re.workbook.sheets[0].drawings[0].kind {
            assert_eq!(cd.kind, "scatter");
            cd.title = "Renamed".into(); // what the panel does
            cd.edited = true;
        }
        let saved = String::from_utf8(
            load_xlsx(&save_xlsx(&re))
                .unwrap()
                .part("xl/charts/chart1.xml")
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            saved.contains("<c:scatterChart>") && saved.contains("<c:xVal>"),
            "a scatter chart must not be rewritten as a column chart: {saved}"
        );
    }

    #[test]
    fn an_edited_stacked_chart_is_kept_verbatim_rather_than_unstacked() {
        use crate::sheet::DrawingKind;
        // A stacked column chart loads as kind "column" — writable by kind — but
        // `chart_space_xml` emits `<c:grouping val="clustered"/>` and no
        // `<c:overlap>`, so regenerating it silently unstacks the plot. That is
        // what `ChartData::complex` is for.
        let mut pkg = new_xlsx();
        pkg.add_chart(
            0,
            (1, 1),
            (10, 6),
            &crate::sheet::ChartData {
                title: "Placeholder".into(),
                kind: "column".into(),
                ..Default::default()
            },
        );
        let stacked = "<?xml version=\"1.0\"?>\n<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\">\
<c:chart><c:plotArea><c:layout/>\
<c:barChart><c:barDir val=\"col\"/><c:grouping val=\"stacked\"/><c:overlap val=\"100\"/>\
<c:ser><c:val><c:numRef><c:f>Sheet1!$B$2:$B$3</c:f></c:numRef></c:val></c:ser>\
</c:barChart></c:plotArea></c:chart></c:chartSpace>";
        if let Some(p) = pkg
            .parts
            .iter_mut()
            .find(|(n, _)| n == "xl/charts/chart1.xml")
        {
            p.1 = stacked.as_bytes().to_vec();
        }
        let mut re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        if let DrawingKind::Chart(cd) = &mut re.workbook.sheets[0].drawings[0].kind {
            assert_eq!(cd.kind, "column", "kind alone would call this writable");
            assert!(cd.complex);
            assert!(!chart_is_writable(cd));
            cd.title = "Renamed".into(); // what the panel does
            cd.edited = true;
        }
        let saved = String::from_utf8(
            load_xlsx(&save_xlsx(&re))
                .unwrap()
                .part("xl/charts/chart1.xml")
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            saved.contains("val=\"stacked\"") && saved.contains("<c:overlap val=\"100\"/>"),
            "a stacked chart must not come back clustered: {saved}"
        );
    }

    #[test]
    fn a_chart_joins_a_drawing_part_whose_root_closes_itself() {
        // A drawing part whose last shape was deleted can be written as a
        // self-closed empty root. Appending the anchor after it would give the
        // part TWO top-level elements — not well-formed, and Excel calls the
        // whole workbook unreadable.
        let mut pkg = new_xlsx();
        pkg.add_chart(0, (1, 1), (10, 6), &crate::sheet::ChartData::default());
        let empty = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"/>";
        if let Some(p) = pkg
            .parts
            .iter_mut()
            .find(|(n, _)| n == "xl/drawings/drawing1.xml")
        {
            p.1 = empty.as_bytes().to_vec();
        }
        pkg.add_chart(0, (12, 1), (20, 6), &crate::sheet::ChartData::default());
        let part = String::from_utf8(pkg.part("xl/drawings/drawing1.xml").unwrap().to_vec())
            .expect("still utf-8");
        assert!(
            part.trim_end().ends_with("</xdr:wsDr>"),
            "the anchor must land INSIDE the root: {part}"
        );
        assert_eq!(
            part.matches("twoCellAnchor").count(),
            2,
            "one open, one close: {part}"
        );
        // And it reloads as one chart on the sheet.
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(re.workbook.sheets[0].drawings.len(), 1);
    }

    #[test]
    fn a_self_closed_root_is_reopened_only_when_it_is_the_named_one() {
        assert_eq!(
            open_self_closed_root("<a/><xdr:wsDr x=\"1\"/>", "xdr:wsDr").as_deref(),
            Some("<a/><xdr:wsDr x=\"1\">")
        );
        assert_eq!(
            open_self_closed_root("<xdr:wsDr/>", "xdr:wsDr").as_deref(),
            Some("<xdr:wsDr>")
        );
        // Already open: the caller's `rfind` of the close tag handles that.
        assert_eq!(
            open_self_closed_root("<xdr:wsDr></xdr:wsDr>", "xdr:wsDr"),
            None
        );
        // A longer name that merely starts the same way.
        assert_eq!(open_self_closed_root("<xdr:wsDrX/>", "xdr:wsDr"), None);
        assert_eq!(open_self_closed_root("", "xdr:wsDr"), None);
    }

    #[test]
    fn a_worksheet_naming_an_unreadable_drawing_is_repointed_at_the_new_one() {
        // The loader sets `drawing_part` only when the rel resolves AND the part
        // reads. When it doesn't, the model has none while the worksheet still
        // carries a `<drawing r:id="…"/>` — and a worksheet may carry only one.
        // Minting a fresh part without repointing that element orphans it: the
        // chart is silently absent from the saved file.
        let mut pkg = new_xlsx();
        let sheet_part = pkg.sheet_parts[0].clone();
        if let Some(p) = pkg.parts.iter_mut().find(|(n, _)| *n == sheet_part) {
            let mut xml = String::from_utf8(p.1.clone()).unwrap();
            let at = xml.find("</worksheet>").unwrap();
            xml.insert_str(at, "<drawing r:id=\"rIdGone\"/>");
            p.1 = xml.into_bytes();
        }
        assert!(pkg.workbook.sheets[0].drawing_part.is_none());
        pkg.add_chart(0, (1, 1), (10, 6), &crate::sheet::ChartData::default());

        let ws = String::from_utf8(pkg.part(&sheet_part).unwrap().to_vec()).unwrap();
        assert_eq!(ws.matches("<drawing ").count(), 1, "still only one: {ws}");
        assert!(!ws.contains("rIdGone"), "the dead rel is gone: {ws}");
        // And the chart survives a round-trip through the file.
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(re.workbook.sheets[0].drawings.len(), 1);
    }

    #[test]
    fn a_spliced_chart_anchor_takes_a_free_shape_id_and_orders_the_worksheet() {
        use crate::sheet::ChartData;
        // `cNvPr/@id` is unique within a drawing part, and Excel's first picture
        // is `id="2"` — the chart part number says nothing about it. And
        // CT_Worksheet is a sequence: `<drawing>` precedes `<tableParts>`.
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, crate::sheet::Cell::text("Item"));
        pkg.workbook.sheets[0].set_cell(1, 0, crate::sheet::Cell::text("Nut"));
        pkg.add_table(0, (0, 0, 1, 0), true, "TableStyleLight1")
            .unwrap();
        // A drawing part already holding a picture at id 2.
        let existing = "<?xml version=\"1.0\"?>\n<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\">\
<xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>0</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>\
<xdr:to><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>\
<xdr:pic><xdr:nvPicPr><xdr:cNvPr id=\"2\" name=\"Picture 1\"/><xdr:cNvPicPr/></xdr:nvPicPr></xdr:pic><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>";
        pkg.parts
            .push(("xl/drawings/drawing1.xml".into(), existing.into()));
        pkg.workbook.sheets[0].drawing_part = Some("xl/drawings/drawing1.xml".into());

        pkg.add_chart(0, (5, 0), (12, 5), &ChartData::default());

        let dx = String::from_utf8(pkg.part("xl/drawings/drawing1.xml").unwrap().to_vec()).unwrap();
        assert_eq!(
            dx.matches("cNvPr id=\"2\"").count(),
            1,
            "the chart must not reuse the picture's shape id: {dx}"
        );
        assert!(dx.contains("cNvPr id=\"3\""), "{dx}");

        let ws = String::from_utf8(pkg.part("xl/worksheets/sheet1.xml").unwrap().to_vec()).unwrap();
        let (d, t) = (
            ws.find("<drawing ").expect("a drawing element"),
            ws.find("<tableParts").expect("a tableParts element"),
        );
        assert!(d < t, "<drawing> must precede <tableParts>: {ws}");
    }

    #[test]
    fn non_finite_numbers_never_reach_the_file() {
        // `{}` prints `NaN`/`inf`, neither of which is an xsd:double. A load can
        // carry one in, since `parse::<f64>()` accepts both.
        assert_eq!(num_repr(f64::NAN), "0");
        assert_eq!(num_repr(f64::INFINITY), "0");
        assert_eq!(num_repr(f64::NEG_INFINITY), "0");
        let xml = chart_space_xml(&crate::sheet::ChartData {
            kind: "column".into(),
            categories: vec!["a".into(), "b".into()],
            series: vec![crate::sheet::ChartSeries {
                name: "S".into(),
                values: vec![f64::NAN, f64::INFINITY],
                ..Default::default()
            }],
            ..Default::default()
        });
        assert!(!xml.contains("NaN") && !xml.contains("inf"), "{xml}");
    }

    /// Exactly what the writer emitted for the Overview's `A1:D4` table read
    /// by column, at the commit before orientation reached the writer —
    /// dumped from that build and diffed against this one, not hand-written.
    const COLUMN_GOLDEN: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><c:chart><c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Item</a:t></a:r></a:p></c:rich></c:tx><c:overlay val=\"0\"/></c:title><c:autoTitleDeleted val=\"0\"/><c:plotArea><c:layout/><c:barChart><c:barDir val=\"col\"/><c:grouping val=\"clustered\"/><c:varyColors val=\"0\"/><c:ser><c:idx val=\"0\"/><c:order val=\"0\"/><c:tx><c:strRef><c:f>Data!$B$1</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>Qty</c:v></c:pt></c:strCache></c:strRef></c:tx><c:cat><c:strRef><c:f>Data!$A$2:$A$4</c:f><c:strCache><c:ptCount val=\"3\"/><c:pt idx=\"0\"><c:v>Laptop</c:v></c:pt><c:pt idx=\"1\"><c:v>Monitor</c:v></c:pt><c:pt idx=\"2\"><c:v>Keyboard</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:f>Data!$B$2:$B$4</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"3\"/><c:pt idx=\"0\"><c:v>2</c:v></c:pt><c:pt idx=\"1\"><c:v>4</c:v></c:pt><c:pt idx=\"2\"><c:v>6</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser><c:ser><c:idx val=\"1\"/><c:order val=\"1\"/><c:tx><c:strRef><c:f>Data!$C$1</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>Unit price</c:v></c:pt></c:strCache></c:strRef></c:tx><c:cat><c:strRef><c:f>Data!$A$2:$A$4</c:f><c:strCache><c:ptCount val=\"3\"/><c:pt idx=\"0\"><c:v>Laptop</c:v></c:pt><c:pt idx=\"1\"><c:v>Monitor</c:v></c:pt><c:pt idx=\"2\"><c:v>Keyboard</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:f>Data!$C$2:$C$4</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"3\"/><c:pt idx=\"0\"><c:v>1199</c:v></c:pt><c:pt idx=\"1\"><c:v>249.5</c:v></c:pt><c:pt idx=\"2\"><c:v>39.99</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser><c:ser><c:idx val=\"2\"/><c:order val=\"2\"/><c:tx><c:strRef><c:f>Data!$D$1</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>Total</c:v></c:pt></c:strCache></c:strRef></c:tx><c:cat><c:strRef><c:f>Data!$A$2:$A$4</c:f><c:strCache><c:ptCount val=\"3\"/><c:pt idx=\"0\"><c:v>Laptop</c:v></c:pt><c:pt idx=\"1\"><c:v>Monitor</c:v></c:pt><c:pt idx=\"2\"><c:v>Keyboard</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:f>Data!$D$2:$D$4</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"3\"/><c:pt idx=\"0\"><c:v>2398</c:v></c:pt><c:pt idx=\"1\"><c:v>998</c:v></c:pt><c:pt idx=\"2\"><c:v>239.94</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser><c:axId val=\"111111111\"/><c:axId val=\"222222222\"/></c:barChart><c:catAx><c:axId val=\"111111111\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"b\"/><c:crossAx val=\"222222222\"/></c:catAx><c:valAx><c:axId val=\"222222222\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"l\"/><c:crossAx val=\"111111111\"/></c:valAx></c:plotArea><c:legend><c:legendPos val=\"b\"/><c:overlay val=\"0\"/></c:legend><c:plotVisOnly val=\"1\"/><c:dispBlanksAs val=\"gap\"/></c:chart></c:chartSpace>";

    /// The Overview's worked example, as a sheet: `A1:D4`, one product per row.
    fn overview_table() -> crate::sheet::Sheet {
        use crate::sheet::{Cell, Sheet, parse_cell_name};
        let mut sh = Sheet {
            name: "Data".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("C1", Cell::text("Unit price")),
            ("D1", Cell::text("Total")),
            ("A2", Cell::text("Laptop")),
            ("B2", Cell::number(2.0)),
            ("C2", Cell::number(1199.0)),
            ("D2", Cell::number(2398.0)),
            ("A3", Cell::text("Monitor")),
            ("B3", Cell::number(4.0)),
            ("C3", Cell::number(249.5)),
            ("D3", Cell::number(998.0)),
            ("A4", Cell::text("Keyboard")),
            ("B4", Cell::number(6.0)),
            ("C4", Cell::number(39.99)),
            ("D4", Cell::number(239.94)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        sh
    }

    /// Every `<c:f>` a chart writes, in document order, tagged with the slot it
    /// sits in — the three ref slots a series has.
    fn f_refs(xml: &str) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        let mut slot = "";
        let mut rest = xml;
        while let Some(i) = rest.find('<') {
            rest = &rest[i..];
            for (tag, name) in [("<c:tx>", "tx"), ("<c:cat>", "cat"), ("<c:val>", "val")] {
                if rest.starts_with(tag) {
                    slot = name;
                }
            }
            if let Some(body) = rest.strip_prefix("<c:f>") {
                let end = body.find("</c:f>").expect("closed <c:f>");
                out.push((slot, body[..end].to_string()));
            }
            rest = &rest[1..];
        }
        out
    }

    #[test]
    fn a_row_oriented_chart_writes_row_shaped_refs() {
        // The whole of orientation lives in these strings: SpreadsheetML has no
        // element saying "by row", so a chart is row-oriented exactly when its
        // `<c:val>` spans one row and its `<c:cat>` names the header row.
        // `to_ref` writes whatever rectangle it is handed, so values and names
        // needed no writer change — this pins that, so a later refactor of the
        // ref path cannot quietly turn them back into columns.
        let sh = overview_table();
        let cd = crate::sheet::chart_from_range(&sh, "Data", (0, 0, 3, 3), "column", true)
            .expect("chart");
        let xml = chart_space_xml(&cd);
        assert_eq!(
            f_refs(&xml),
            vec![
                ("tx", "Data!$A$2".to_string()),
                ("cat", "Data!$B$1:$D$1".to_string()),
                ("val", "Data!$B$2:$D$2".to_string()),
                ("tx", "Data!$A$3".to_string()),
                ("cat", "Data!$B$1:$D$1".to_string()),
                ("val", "Data!$B$3:$D$3".to_string()),
                ("tx", "Data!$A$4".to_string()),
                ("cat", "Data!$B$1:$D$1".to_string()),
                ("val", "Data!$B$4:$D$4".to_string()),
            ],
            "{xml}"
        );
        // The caches beside those refs say what the refs say.
        assert!(
            xml.contains("<c:pt idx=\"0\"><c:v>Laptop</c:v></c:pt>")
                && xml.contains("<c:pt idx=\"0\"><c:v>Qty</c:v></c:pt>")
                && xml.contains("<c:pt idx=\"2\"><c:v>2398</c:v></c:pt>"),
            "{xml}"
        );
    }

    #[test]
    fn a_column_oriented_chart_writes_exactly_what_it_wrote_before_orientation() {
        // The regression that matters most: every chart in every existing file
        // is column-oriented, so the row work must not move a byte of what a
        // column chart emits. If this fails the answer is to fix the writer,
        // not to re-bless the string.
        let sh = overview_table();
        let cd = crate::sheet::chart_from_range(&sh, "Data", (0, 0, 3, 3), "column", false)
            .expect("chart");
        assert_eq!(chart_space_xml(&cd), COLUMN_GOLDEN);
    }

    #[test]
    fn a_row_chart_with_no_category_ref_writes_literal_labels_not_a_column_of_names() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource};
        // A row chart whose series were named by hand carries no `name_ref`, so
        // nothing claims column A — the column fallback would have found
        // `cat_col` free and handed Excel `Data!$A$2:$A$3`, a column of series
        // NAMES, as the category labels. `by_row` turns that fallback off and
        // the labels go out as literals instead.
        let row = |r| ChartSource {
            sheet: "Data".into(),
            range: (r, 1, r, 3),
            cat_col: 0,
        };
        let cd = ChartData {
            kind: "column".into(),
            categories: vec!["Qty".into(), "Unit price".into(), "Total".into()],
            series: vec![
                ChartSeries {
                    name: "Laptop".into(),
                    values: vec![2.0, 1199.0, 2398.0],
                    col: None,
                    values_ref: Some(row(1)),
                    ..Default::default()
                },
                ChartSeries {
                    name: "Monitor".into(),
                    values: vec![4.0, 249.5, 998.0],
                    col: None,
                    values_ref: Some(row(2)),
                    ..Default::default()
                },
            ],
            source: Some(ChartSource {
                sheet: "Data".into(),
                range: (0, 0, 2, 3),
                cat_col: 0,
            }),
            categories_ref: None,
            edited: true,
            by_row: true,
            ..Default::default()
        };
        let xml = chart_space_xml(&cd);
        assert!(xml.contains("<c:cat><c:strLit"), "{xml}");
        assert!(!xml.contains("$A$2:$A$3"), "column fallback fired: {xml}");
        // Only the values are refs; `<c:tx>` is a literal `<c:v>`, since these
        // series carry no `name_ref`.
        assert_eq!(
            f_refs(&xml),
            vec![
                ("val", "Data!$B$2:$D$2".to_string()),
                ("val", "Data!$B$3:$D$3".to_string()),
            ],
            "{xml}"
        );
    }

    #[test]
    fn the_column_fallback_still_derives_a_category_ref_when_it_should() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource};
        // The other side of the guard: a COLUMN chart with no `categories_ref`
        // and an unclaimed label column still derives one, exactly as before.
        let cd = ChartData {
            kind: "column".into(),
            categories: vec!["a".into(), "b".into(), "c".into()],
            series: vec![ChartSeries {
                name: "S".into(),
                values: vec![1.0, 2.0, 3.0],
                col: Some(1),
                ..Default::default()
            }],
            source: Some(ChartSource {
                sheet: "Data".into(),
                range: (0, 0, 3, 1),
                cat_col: 0,
            }),
            categories_ref: None,
            edited: true,
            ..Default::default()
        };
        let xml = chart_space_xml(&cd);
        assert!(
            xml.contains("<c:cat><c:strRef><c:f>Data!$A$2:$A$4</c:f>"),
            "{xml}"
        );
    }

    #[test]
    fn loaded_numbers_keep_every_digit() {
        // #655: only typed entry truncates to 15 digits; a number read from a
        // file keeps its full double.
        let rows = r#"<row r="1"><c r="A1"><v>1234567890123456789</v></c></row>"#;
        let pkg = load_xlsx(&cell_meta_fixture(rows)).unwrap();
        assert_eq!(
            pkg.workbook.sheets[0].cell(0, 0).unwrap().value,
            crate::sheet::CellValue::Number(1234567890123456789.0)
        );
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;

    const XLSX_CT: &str =
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";
    const XLSM_CT: &str = "application/vnd.ms-excel.sheet.macroEnabled.main+xml";
    const XLTX_CT: &str =
        "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml";
    const XLTM_CT: &str = "application/vnd.ms-excel.template.macroEnabled.main+xml";

    fn content_types(pkg: &SheetPackage) -> String {
        String::from_utf8_lossy(pkg.part("[Content_Types].xml").unwrap()).into_owned()
    }

    fn workbook_ct(pkg: &SheetPackage) -> String {
        let xml = content_types(pkg);
        let el = override_element(&xml, "/xl/workbook.xml").expect("workbook Override");
        let ct = find_element_by_attr(&xml[el.start..el.end], "Override", "ContentType", |_| true)
            .unwrap();
        xml[el.start + ct.value.0..el.start + ct.value.1].to_string()
    }

    fn retyped(pkg: &SheetPackage, from: &str, to: &str) -> SheetPackage {
        let mut pkg = pkg.clone();
        let ct = content_types(&pkg).replace(from, to);
        pkg.set_part("[Content_Types].xml", ct.into_bytes());
        pkg
    }

    /// A template, as Excel (or openpyxl with `template = True`) writes one.
    fn xltx() -> SheetPackage {
        retyped(&new_xlsx(), XLSX_CT, XLTX_CT)
    }

    /// A macro workbook as Excel writes one: the VBA project behind the `.bin`
    /// Default, a signature named from the project's own rels, and printer
    /// settings — another `.bin` that has its own Override and must survive.
    fn xlsm() -> SheetPackage {
        let mut pkg = retyped(&new_xlsx(), XLSX_CT, XLSM_CT);
        let ct = content_types(&pkg).replace(
            r#"<Default Extension="xml""#,
            r#"<Default Extension="bin" ContentType="application/vnd.ms-office.vbaProject"/><Default Extension="xml""#,
        );
        let ct = ct.replace(
            "</Types>",
            r#"<Override PartName="/xl/vbaProjectSignature.bin" ContentType="application/vnd.ms-office.vbaProjectSignature"/><Override PartName="/xl/printerSettings/printerSettings1.bin" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.printerSettings"/></Types>"#,
        );
        pkg.set_part("[Content_Types].xml", ct.into_bytes());
        let rels = String::from_utf8_lossy(pkg.part("xl/_rels/workbook.xml.rels").unwrap())
            .replace(
                "</Relationships>",
                r#"<Relationship Id="rId9" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="vbaProject.bin"/></Relationships>"#,
            );
        pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());
        pkg.set_part("xl/vbaProject.bin", b"VBA".to_vec());
        pkg.set_part(
            "xl/_rels/vbaProject.bin.rels",
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProjectSignature" Target="vbaProjectSignature.bin"/></Relationships>"#
                .to_vec(),
        );
        pkg.set_part("xl/vbaProjectSignature.bin", b"SIG".to_vec());
        pkg.set_part(
            "xl/printerSettings/printerSettings1.bin",
            b"DEVMODE".to_vec(),
        );
        pkg
    }

    fn roundtrip(pkg: &SheetPackage, kind: SpreadsheetKind) -> SheetPackage {
        load_xlsx(&save_xlsx_as(pkg, kind)).expect("the saved file reloads")
    }

    #[test]
    fn kinds_come_from_the_extension_in_any_case() {
        use SpreadsheetKind::*;
        assert_eq!(SpreadsheetKind::from_path("a/b.XLSX"), Some(Workbook));
        assert_eq!(SpreadsheetKind::from_path("b.xlsm"), Some(MacroWorkbook));
        assert_eq!(SpreadsheetKind::from_path("b.Xltx"), Some(Template));
        assert_eq!(SpreadsheetKind::from_path("b.xltm"), Some(MacroTemplate));
        assert_eq!(SpreadsheetKind::from_path("b.csv"), None);
        assert_eq!(SpreadsheetKind::from_path("b"), None);
        assert!(MacroWorkbook.allows_macros() && MacroTemplate.allows_macros());
        assert!(!Workbook.allows_macros() && !Template.allows_macros());
        assert_eq!(Workbook.main_content_type(), XLSX_CT);
        assert_eq!(MacroWorkbook.main_content_type(), XLSM_CT);
        assert_eq!(Template.main_content_type(), XLTX_CT);
        assert_eq!(MacroTemplate.main_content_type(), XLTM_CT);
    }

    /// #601: a template saved as `.xlsx` must say it is a workbook, or Excel
    /// refuses the file.
    #[test]
    fn a_template_saved_as_xlsx_is_a_workbook() {
        let out = roundtrip(&xltx(), SpreadsheetKind::Workbook);
        assert_eq!(workbook_ct(&out), XLSX_CT);
        assert!(!content_types(&out).contains("template"));
    }

    /// #601: a macro workbook saved as `.xlsx` loses the macro type and the
    /// whole VBA project, but nothing else.
    #[test]
    fn a_macro_workbook_saved_as_xlsx_drops_the_vba_project() {
        let pkg = xlsm();
        assert!(pkg.has_vba_project());
        let out = roundtrip(&pkg, SpreadsheetKind::Workbook);
        assert_eq!(workbook_ct(&out), XLSX_CT);
        assert!(!out.has_vba_project());
        for gone in [
            "xl/vbaProject.bin",
            "xl/_rels/vbaProject.bin.rels",
            "xl/vbaProjectSignature.bin",
        ] {
            assert!(out.part(gone).is_none(), "{gone} survived");
        }
        let rels =
            String::from_utf8_lossy(out.part("xl/_rels/workbook.xml.rels").unwrap()).into_owned();
        assert!(!rels.contains("vbaProject"), "{rels}");
        assert!(rels.contains("<Relationships "), "rels root intact: {rels}");
        assert!(rels.contains("sharedStrings"), "other rels intact: {rels}");
        let ct = content_types(&out);
        assert!(!ct.contains("vbaProject"), "{ct}");
        assert!(!ct.contains("macroEnabled"), "{ct}");
        // Printer settings are not macros.
        assert_eq!(
            out.part("xl/printerSettings/printerSettings1.bin"),
            Some(&b"DEVMODE"[..])
        );
        assert!(ct.contains("/xl/printerSettings/printerSettings1.bin"));
        // The package we saved from still has its macros.
        assert!(pkg.has_vba_project());
    }

    /// The `.bin` Default stays while a `.bin` part without an Override of its
    /// own still relies on it.
    #[test]
    fn the_bin_default_stays_while_another_bin_part_relies_on_it() {
        let mut pkg = xlsm();
        let ct = content_types(&pkg).replace(
            r#"<Override PartName="/xl/printerSettings/printerSettings1.bin" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.printerSettings"/>"#,
            "",
        );
        pkg.set_part("[Content_Types].xml", ct.into_bytes());
        let out = roundtrip(&pkg, SpreadsheetKind::Workbook);
        assert!(content_types(&out).contains(r#"<Default Extension="bin""#));
        assert!(out.part("xl/vbaProject.bin").is_none());
    }

    #[test]
    fn saving_to_the_same_type_keeps_type_and_parts() {
        let m = xlsm();
        let out = roundtrip(&m, SpreadsheetKind::MacroWorkbook);
        assert_eq!(workbook_ct(&out), XLSM_CT);
        assert!(out.has_vba_project());
        assert_eq!(out.part("xl/vbaProjectSignature.bin"), Some(&b"SIG"[..]));
        assert_eq!(content_types(&out), content_types(&m));

        let out = roundtrip(&m, SpreadsheetKind::MacroTemplate);
        assert_eq!(workbook_ct(&out), XLTM_CT);
        assert!(out.has_vba_project());

        let t = xltx();
        let out = roundtrip(&t, SpreadsheetKind::Template);
        assert_eq!(workbook_ct(&out), XLTX_CT);
        assert_eq!(content_types(&out), content_types(&t));
    }

    #[test]
    fn a_workbook_can_be_saved_as_any_kind() {
        for (kind, ct) in [
            (SpreadsheetKind::Template, XLTX_CT),
            (SpreadsheetKind::MacroTemplate, XLTM_CT),
            (SpreadsheetKind::MacroWorkbook, XLSM_CT),
        ] {
            let out = roundtrip(&new_xlsx(), kind);
            assert_eq!(workbook_ct(&out), ct);
            assert_eq!(content_types(&out).matches("/xl/workbook.xml").count(), 1);
        }
    }

    /// A workbook typed only by the `xml` Default gets an Override of its own.
    #[test]
    fn a_workbook_without_an_override_gets_one() {
        let mut pkg = new_xlsx();
        let xml = content_types(&pkg);
        let el = override_element(&xml, "/xl/workbook.xml").unwrap();
        pkg.set_part(
            "[Content_Types].xml",
            format!("{}{}", &xml[..el.start], &xml[el.end..]).into_bytes(),
        );
        let out = roundtrip(&pkg, SpreadsheetKind::Workbook);
        assert_eq!(workbook_ct(&out), XLSX_CT);
    }

    /// OPC part names compare case-insensitively.
    #[test]
    fn the_workbook_override_is_found_in_any_case() {
        let mut pkg = xltx();
        let ct = content_types(&pkg).replace("/xl/workbook.xml", "/XL/Workbook.xml");
        pkg.set_part("[Content_Types].xml", ct.into_bytes());
        let out = save_xlsx_as(&pkg, SpreadsheetKind::Workbook);
        let ct = content_types(&load_xlsx(&out).unwrap());
        assert!(!ct.contains("template"), "{ct}");
        assert_eq!(
            ct.to_ascii_lowercase().matches("/xl/workbook.xml").count(),
            1
        );
    }

    /// Other writers use single quotes and `></X>` closes. The workbook's
    /// Override is still found (not duplicated beside the stale macro type),
    /// and the VBA relationship goes without taking a neighbour with it.
    #[test]
    fn single_quotes_and_explicit_close_tags_are_read() {
        let mut pkg = xlsm();
        let ct = content_types(&pkg).replace('"', "'");
        pkg.set_part("[Content_Types].xml", ct.into_bytes());
        let rels = String::from_utf8_lossy(pkg.part("xl/_rels/workbook.xml.rels").unwrap())
            .replace('"', "'")
            .replace("/>", "></Relationship>");
        pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());
        assert!(pkg.has_vba_project());

        let out = roundtrip(&pkg, SpreadsheetKind::Workbook);
        let ct = content_types(&out);
        assert_eq!(ct.matches("/xl/workbook.xml").count(), 1, "{ct}");
        assert_eq!(workbook_ct(&out), XLSX_CT);
        assert!(
            !ct.contains("macroEnabled") && !ct.contains("vbaProject"),
            "{ct}"
        );
        assert!(ct.contains("<Default Extension='xml'"), "{ct}");
        assert!(out.part("xl/vbaProject.bin").is_none());
        let rels =
            String::from_utf8_lossy(out.part("xl/_rels/workbook.xml.rels").unwrap()).into_owned();
        assert!(!rels.contains("vbaProject"), "{rels}");
        assert_eq!(rels.matches("<Relationship ").count(), 3, "{rels}");
        assert_eq!(out.workbook.sheets.len(), 1);
    }

    /// The span is exactly the matched element: a neighbour closed with
    /// `></X>` is not swallowed, and values compare decoded.
    #[test]
    fn the_element_span_is_exactly_the_match() {
        let mut xml = String::from(
            r#"<R><Relationship Id='a'></Relationship><Relationship Id="r&amp;9" Target="x"/></R>"#,
        );
        assert!(find_element_by_attr(&xml, "Relationship", "Id", |v| v == "r9").is_none());
        let el = find_element_by_attr(&xml, "Relationship", "Id", |v| v == "r&9").unwrap();
        xml.replace_range(el.start..el.end, "");
        assert_eq!(xml, "<R><Relationship Id='a'></Relationship></R>");
        let el = find_element_by_attr(&xml, "Relationship", "Id", |v| v == "a").unwrap();
        xml.replace_range(el.start..el.end, "");
        assert_eq!(xml, "<R></R>");
    }

    /// A commented-out copy ahead of the real element is not the element:
    /// the live relationship goes, and the live Override is the one retyped.
    #[test]
    fn commented_out_copies_are_not_elements() {
        let mut pkg = xlsm();
        let ct = content_types(&pkg).replace(
            "<Default Extension=\"rels\"",
            &format!(
                "<!-- <Override PartName=\"/xl/workbook.xml\" ContentType=\"{XLSM_CT}\"/> --><Default Extension=\"rels\""
            ),
        );
        pkg.set_part("[Content_Types].xml", ct.into_bytes());
        let rels = String::from_utf8_lossy(pkg.part("xl/_rels/workbook.xml.rels").unwrap())
            .replace(
                "<Relationship Id=\"rId1\"",
                "<!-- <Relationship Id=\"rId9\" Type=\"x\" Target=\"old.bin\"/> --><Relationship Id=\"rId1\"",
            );
        pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());

        let out = roundtrip(&pkg, SpreadsheetKind::Workbook);
        let ct = content_types(&out);
        let live = ct.rsplit("-->").next().unwrap();
        assert!(
            !live.contains("macroEnabled"),
            "the live Override was retyped: {ct}"
        );
        assert!(live.contains(XLSX_CT), "{ct}");
        let rels =
            String::from_utf8_lossy(out.part("xl/_rels/workbook.xml.rels").unwrap()).into_owned();
        let live = rels.rsplit("-->").next().unwrap();
        assert!(
            !live.contains("vbaProject"),
            "the live relationship went: {rels}"
        );
        assert!(!out.has_vba_project());
        assert!(out.part("xl/vbaProject.bin").is_none());
    }

    #[test]
    fn remove_vba_project_strips_it_in_memory() {
        let mut pkg = xlsm();
        assert!(pkg.remove_vba_project());
        assert!(!pkg.has_vba_project());
        assert!(pkg.part("xl/vbaProject.bin").is_none());
        assert!(!pkg.remove_vba_project());
        // save_xlsx keeps the loaded type: only the kind-aware save retypes.
        assert_eq!(workbook_ct(&load_xlsx(&save_xlsx(&pkg)).unwrap()), XLSM_CT);
    }

    #[test]
    fn save_for_path_keeps_the_loaded_type_for_other_extensions() {
        let out = load_xlsx(&save_xlsx_for_path(&xltx(), "out.bak")).unwrap();
        assert_eq!(workbook_ct(&out), XLTX_CT);
        let out = load_xlsx(&save_xlsx_for_path(&xltx(), "out.XLSX")).unwrap();
        assert_eq!(workbook_ct(&out), XLSX_CT);
    }

    const MACROSHEET_CT: &str = "application/vnd.ms-excel.macrosheet+xml";
    const DIALOGSHEET_CT: &str =
        "application/vnd.openxmlformats-officedocument.spreadsheetml.dialogsheet+xml";

    /// An `.xlsm` with Excel 4.0 macro and dialog sheets between worksheets:
    /// `Data`, `Macro1` (an XLM macro sheet, the first workbook relationship,
    /// with printer settings), `Dialog1`, and `Report` (active). Names: a
    /// global `Auto_Open` on the macro sheet, an `xlm` name, a plain `Total`,
    /// and `Report`'s print area (`localSheetId` 3).
    fn xlm_book(sheets: &[(&str, &str)]) -> SheetPackage {
        let main = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        let od = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let ms = "http://schemas.microsoft.com/office/2006/relationships";
        let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
        let mut sheet_els = String::new();
        let mut rels = String::new();
        let mut overrides = String::new();
        let mut active = 0;
        for (i, (name, kind)) in sheets.iter().enumerate() {
            let n = i + 1;
            let (ty, dir, ct, body) = match *kind {
                "macro" => (
                    format!("{ms}/xlMacrosheet"),
                    "macrosheets",
                    MACROSHEET_CT,
                    format!(
                        "<xm:macrosheet xmlns=\"{main}\" xmlns:xm=\"http://schemas.microsoft.com/office/excel/2006/main\"><sheetData><row r=\"1\"><c r=\"A1\"><f>ALERT(\"hi\")</f></c></row><row r=\"2\"><c r=\"A2\"><f>RETURN()</f></c></row></sheetData></xm:macrosheet>"
                    ),
                ),
                "dialog" => (
                    format!("{od}/dialogsheet"),
                    "dialogsheets",
                    DIALOGSHEET_CT,
                    format!(
                        "<dialogsheet xmlns=\"{main}\"><sheetViews><sheetView workbookViewId=\"0\"/></sheetViews></dialogsheet>"
                    ),
                ),
                _ => (
                    format!("{od}/worksheet"),
                    "worksheets",
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
                    format!(
                        "<worksheet xmlns=\"{main}\"><sheetData><row r=\"1\"><c r=\"A1\"><v>{n}</v></c></row></sheetData></worksheet>"
                    ),
                ),
            };
            if *name == "Report" {
                active = i;
            }
            sheet_els.push_str(&format!(
                "<sheet name=\"{name}\" sheetId=\"{n}\" r:id=\"rId{n}\"/>"
            ));
            let rel = format!(
                "<Relationship Id=\"rId{n}\" Type=\"{ty}\" Target=\"{dir}/sheet{n}.xml\"/>"
            );
            // Macro sheets lead the rels, so one is the first child.
            if *kind == "macro" {
                rels.insert_str(0, &rel);
            } else {
                rels.push_str(&rel);
            }
            overrides.push_str(&format!(
                "<Override PartName=\"/xl/{dir}/sheet{n}.xml\" ContentType=\"{ct}\"/>"
            ));
            parts.push((format!("xl/{dir}/sheet{n}.xml"), body.into_bytes()));
            if *kind == "macro" {
                parts.push((
                    format!("xl/{dir}/_rels/sheet{n}.xml.rels"),
                    format!("<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{od}/printerSettings\" Target=\"../printerSettings/printerSettings{n}.bin\"/></Relationships>").into_bytes(),
                ));
                parts.push((
                    format!("xl/printerSettings/printerSettings{n}.bin"),
                    b"DEVMODE".to_vec(),
                ));
                overrides.push_str(&format!("<Override PartName=\"/xl/printerSettings/printerSettings{n}.bin\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.printerSettings\"/>"));
            }
        }
        let report = sheets.iter().position(|(n, _)| *n == "Report");
        let print_area = report
            .map(|i| format!("<definedName name=\"_xlnm.Print_Area\" localSheetId=\"{i}\">Report!$A$1:$B$2</definedName>"))
            .unwrap_or_default();
        let has_data = sheets.iter().any(|(n, _)| *n == "Data");
        let total = if has_data {
            "<definedName name=\"Total\">Data!$A$1</definedName>"
        } else {
            ""
        };
        let workbook = format!(
            "<workbook xmlns=\"{main}\" xmlns:r=\"{od}\"><bookViews><workbookView activeTab=\"{active}\"/></bookViews><sheets>{sheet_els}</sheets><definedNames>\
             <definedName name=\"Auto_Open\">Macro1!$A$1</definedName>\
             <definedName name=\"CellColor\" xlm=\"1\">GET.CELL(63,INDIRECT(\"rc\",FALSE))</definedName>\
             {total}{print_area}</definedNames></workbook>"
        );
        parts.push(("xl/workbook.xml".into(), workbook.into_bytes()));
        parts.push((
            "xl/_rels/workbook.xml.rels".into(),
            format!("<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rels}</Relationships>").into_bytes(),
        ));
        parts.push((
            "_rels/.rels".into(),
            format!("<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{od}/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>").into_bytes(),
        ));
        parts.push((
            "[Content_Types].xml".into(),
            format!("<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"{XLSM_CT}\"/>{overrides}</Types>").into_bytes(),
        ));
        load_xlsx(&write_zip(&parts)).expect("the XLM fixture loads")
    }

    fn mixed_xlm_book() -> SheetPackage {
        xlm_book(&[
            ("Data", "work"),
            ("Macro1", "macro"),
            ("Dialog1", "dialog"),
            ("Report", "work"),
        ])
    }

    fn part_names(pkg: &SheetPackage) -> Vec<String> {
        pkg.parts.iter().map(|(n, _)| n.clone()).collect()
    }

    #[test]
    fn macro_free_save_drops_macro_and_dialog_sheets() {
        let pkg = mixed_xlm_book();
        assert!(pkg.has_macro_sheets());
        assert_eq!(pkg.workbook.sheets.len(), 4);
        // Macro1 (rId2) is the first workbook relationship.
        assert!(part_text(&pkg, "xl/_rels/workbook.xml.rels").contains(
            "relationships\"><Relationship Id=\"rId2\" Type=\"http://schemas.microsoft.com/office/2006/relationships/xlMacrosheet\""
        ));
        for kind in [SpreadsheetKind::Workbook, SpreadsheetKind::Template] {
            let out = roundtrip(&pkg, kind);
            assert!(!out.has_macro_sheets());
            let names: Vec<&str> = out
                .workbook
                .sheets
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            assert_eq!(names, ["Data", "Report"]);
            assert_eq!(out.workbook.active_tab, 1, "Report stays active");
            let parts = part_names(&out);
            assert!(
                !parts.iter().any(|n| n.contains("macrosheets")
                    || n.contains("dialogsheets")
                    || n.contains("printerSettings")),
                "{parts:?}"
            );
            let ct = content_types(&out);
            assert!(
                !ct.contains(MACROSHEET_CT) && !ct.contains(DIALOGSHEET_CT),
                "{ct}"
            );
            assert!(!ct.contains("printerSettings"), "{ct}");
            let rels = part_text(&out, "xl/_rels/workbook.xml.rels");
            assert!(rels.contains("<Relationships xmlns="), "{rels}");
            assert!(!rels.contains("Macrosheet") && !rels.contains("dialogsheet"));
            let wb = part_text(&out, "xl/workbook.xml");
            assert!(
                wb.contains("<definedName name=\"_xlnm.Print_Area\" localSheetId=\"1\">"),
                "{wb}"
            );
        }
    }

    #[test]
    fn macro_free_save_drops_xlm_and_macro_sheet_names() {
        let out = roundtrip(&mixed_xlm_book(), SpreadsheetKind::Workbook);
        let wb = part_text(&out, "xl/workbook.xml");
        assert!(
            !wb.contains("Auto_Open") && !wb.contains("CellColor"),
            "{wb}"
        );
        assert!(
            wb.contains("<definedName name=\"Total\">Data!$A$1</definedName>"),
            "{wb}"
        );
        let names: Vec<&str> = out
            .workbook
            .defined_names
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert!(names.contains(&"Total"), "{names:?}");
        assert!(!names.contains(&"Auto_Open") && !names.contains(&"CellColor"));

        // The in-memory drop agrees with the file.
        let mut pkg = mixed_xlm_book();
        assert!(pkg.remove_excel4_macros());
        assert!(!pkg.remove_excel4_macros());
        assert!(
            pkg.workbook
                .defined_names
                .iter()
                .all(|d| d.name != "Auto_Open")
        );
    }

    #[test]
    fn macro_save_keeps_macro_sheets() {
        let pkg = mixed_xlm_book();
        for kind in [
            SpreadsheetKind::MacroWorkbook,
            SpreadsheetKind::MacroTemplate,
        ] {
            let bytes = save_xlsx_as(&pkg, kind);
            let out = load_xlsx(&bytes).unwrap();
            assert!(out.has_macro_sheets());
            assert_eq!(out.workbook.sheets.len(), 4);
            // The macro and dialog sheet parts are written as save_xlsx
            // writes them.
            let plain = load_xlsx(&save_xlsx(&pkg)).unwrap();
            for part in ["xl/macrosheets/sheet2.xml", "xl/dialogsheets/sheet3.xml"] {
                assert_eq!(out.part(part), plain.part(part), "{part}");
            }
            assert!(part_text(&out, "xl/workbook.xml").contains("Auto_Open"));
        }
    }

    #[test]
    fn save_xlsx_as_leaves_pkg_untouched_with_macro_sheets() {
        let pkg = mixed_xlm_book();
        let before = (
            part_names(&pkg),
            pkg.parts.clone(),
            pkg.workbook.sheets.len(),
        );
        let _ = save_xlsx_as(&pkg, SpreadsheetKind::Workbook);
        assert_eq!(
            (
                part_names(&pkg),
                pkg.parts.clone(),
                pkg.workbook.sheets.len()
            ),
            before
        );
        assert!(pkg.has_macro_sheets());
    }

    #[test]
    fn only_macro_sheets_get_a_blank_worksheet() {
        let pkg = xlm_book(&[("Macro1", "macro"), ("Dialog1", "dialog")]);
        let out = roundtrip(&pkg, SpreadsheetKind::Workbook);
        assert!(!out.has_macro_sheets());
        assert_eq!(out.workbook.sheets.len(), 1);
        assert_eq!(out.workbook.sheets[0].name, "Sheet1");
        assert!(out.sheet_parts[0].starts_with("xl/worksheets/"));
    }

    fn part_text(pkg: &SheetPackage, name: &str) -> String {
        String::from_utf8_lossy(pkg.part(name).unwrap()).into_owned()
    }

    #[test]
    fn a_formula_refers_to_a_sheet_only_by_its_whole_name() {
        assert!(formula_refers_to_sheet("Macro1!$A$1", "Macro1"));
        assert!(formula_refers_to_sheet("SUM(macro1!A1:A2)", "Macro1"));
        assert!(formula_refers_to_sheet("'My Macros'!$A$1", "My Macros"));
        assert!(formula_refers_to_sheet("'O''Brien'!A1", "O'Brien"));
        assert!(!formula_refers_to_sheet("XMacro1!$A$1", "Macro1"));
        assert!(!formula_refers_to_sheet("'Old Macro1'!$A$1", "Macro1"));
        assert!(!formula_refers_to_sheet("Data!$A$1", "Macro1"));
        // Another workbook's sheet of the same name is not this one.
        assert!(!formula_refers_to_sheet("[1]Macro1!$A$1", "Macro1"));
        assert!(!formula_refers_to_sheet("'[1]My Macros'!$A$1", "My Macros"));
        // Nor is text that only looks like a reference.
        assert!(!formula_refers_to_sheet("\"Macro1!A1\"", "Macro1"));
        assert!(!formula_refers_to_sheet("\"'My Macros'!A1\"", "My Macros"));
        assert!(!formula_refers_to_sheet(
            "\"say \"\"Macro1!\"\"\"",
            "Macro1"
        ));
        assert!(formula_refers_to_sheet(
            "IF(\"x\"=\"x\",Macro1!A1)",
            "Macro1"
        ));
    }

    /// #727 r1: an Excel 4.0 name goes from a macro-free file even when the
    /// workbook has no macro sheet, and stays in a macro-enabled one.
    #[test]
    fn an_xlm_name_without_a_macro_sheet_is_dropped_from_a_macro_free_save() {
        let pkg = xlm_book(&[("Data", "work"), ("Report", "work")]);
        assert!(!pkg.has_macro_sheets());
        assert!(pkg.has_macro_names());
        assert_eq!(
            pkg.macro_features(),
            ["Excel 4.0 function stored in defined names"]
        );
        let out = roundtrip(&pkg, SpreadsheetKind::Workbook);
        let wb = part_text(&out, "xl/workbook.xml");
        assert!(!wb.contains("CellColor"), "{wb}");
        assert!(wb.contains("name=\"Total\""), "{wb}");
        assert!(!out.has_macro_names());
        assert!(out.macro_features().is_empty());
        assert_eq!(out.workbook.sheets.len(), 2);

        let kept = roundtrip(&pkg, SpreadsheetKind::MacroWorkbook);
        assert!(part_text(&kept, "xl/workbook.xml").contains("CellColor"));

        // The in-memory drop does the same.
        let mut copy = pkg.clone();
        assert!(copy.remove_excel4_macros());
        assert!(!copy.has_macro_names());
        assert_eq!(copy.workbook.sheets.len(), 2);
        assert!(!copy.remove_excel4_macros());
    }

    /// #727 r2: a macro sheet named by an absolute or `./` Target is removed
    /// with its relationship and `<sheet>`, not left pointing at nothing.
    #[test]
    fn macro_sheets_with_absolute_or_dotted_targets_are_removed_whole() {
        let mut pkg = mixed_xlm_book();
        let rels = part_text(&pkg, "xl/_rels/workbook.xml.rels")
            .replace(
                "Target=\"macrosheets/sheet2.xml\"",
                "Target=\"/xl/macrosheets/sheet2.xml\"",
            )
            .replace(
                "Target=\"dialogsheets/sheet3.xml\"",
                "Target=\"./dialogsheets/sheet3.xml\"",
            );
        assert!(rels.contains("/xl/macrosheets/") && rels.contains("./dialogsheets/"));
        pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());
        let pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(pkg.workbook.sheets.len(), 4);
        assert!(pkg.has_macro_sheets());

        let out = roundtrip(&pkg, SpreadsheetKind::Workbook);
        let names: Vec<&str> = out
            .workbook
            .sheets
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, ["Data", "Report"]);
        let rels = part_text(&out, "xl/_rels/workbook.xml.rels");
        assert!(
            !rels.contains("macrosheets") && !rels.contains("dialogsheets"),
            "{rels}"
        );
        let wb = part_text(&out, "xl/workbook.xml");
        assert_eq!(wb.matches("<sheet ").count(), 2, "{wb}");
        assert!(!wb.contains("Macro1") && !wb.contains("Dialog1"), "{wb}");
    }

    /// [`mixed_xlm_book`] with its workbook part at `wb/book.xml` (and its
    /// rels at `wb/_rels/book.xml.rels`), where only the package rels say
    /// to look.
    fn relocated_xlm_book() -> SheetPackage {
        let parts: Vec<(String, Vec<u8>)> = mixed_xlm_book()
            .parts
            .into_iter()
            .map(|(name, bytes)| {
                let text = || String::from_utf8_lossy(&bytes).into_owned();
                match name.as_str() {
                    "xl/workbook.xml" => ("wb/book.xml".to_string(), bytes),
                    "xl/_rels/workbook.xml.rels" => (
                        "wb/_rels/book.xml.rels".to_string(),
                        text().replace("Target=\"", "Target=\"../xl/").into_bytes(),
                    ),
                    "_rels/.rels" => (
                        name,
                        text()
                            .replace("xl/workbook.xml", "wb/book.xml")
                            .into_bytes(),
                    ),
                    "[Content_Types].xml" => (
                        name,
                        text()
                            .replace("/xl/workbook.xml", "/wb/book.xml")
                            .into_bytes(),
                    ),
                    _ => (name, bytes),
                }
            })
            .collect();
        let pkg = load_xlsx(&write_zip(&parts)).expect("the relocated fixture loads");
        assert!(pkg.part("xl/workbook.xml").is_none());
        assert_eq!(pkg.workbook.sheets.len(), 4);
        pkg
    }

    /// #789: a workbook part outside `xl/` is found through the package
    /// rels by the sheet removal and the Excel 4.0 macro paths.
    #[test]
    fn remove_sheet_follows_workbook_part_from_rels() {
        let mut pkg = relocated_xlm_book();
        assert!(pkg.has_macro_names());
        assert!(
            pkg.macro_features()
                .contains(&"Excel 4.0 function stored in defined names")
        );

        assert!(pkg.remove_sheet(1));
        let wb = part_text(&pkg, "wb/book.xml");
        assert_eq!(wb.matches("<sheet ").count(), 3, "{wb}");
        assert!(!wb.contains("name=\"Macro1\""), "{wb}");
        let rels = part_text(&pkg, "wb/_rels/book.xml.rels");
        assert!(!rels.contains("macrosheets"), "{rels}");
        assert!(rels.contains("dialogsheets"), "{rels}");

        let mut copy = relocated_xlm_book();
        assert!(copy.remove_excel4_macros());
        let wb = part_text(&copy, "wb/book.xml");
        assert_eq!(wb.matches("<sheet ").count(), 2, "{wb}");
        assert!(!wb.contains("Macro1") && !wb.contains("Dialog1"), "{wb}");
        assert!(
            !wb.contains("CellColor") && !wb.contains("Auto_Open"),
            "{wb}"
        );
        assert!(wb.contains("name=\"Total\""), "{wb}");
        let rels = part_text(&copy, "wb/_rels/book.xml.rels");
        assert!(
            !rels.contains("macrosheets") && !rels.contains("dialogsheets"),
            "{rels}"
        );
        assert!(!copy.has_macro_names() && !copy.has_macro_sheets());
    }

    /// #789: a macro-free save turns worksheet formulas that name a removed
    /// Excel 4.0 macro sheet into `#REF!`, leaves every other formula's text
    /// as it was, and leaves the open package alone.
    #[test]
    fn macro_free_save_turns_macro_sheet_refs_into_ref_errors() {
        let mut pkg = mixed_xlm_book();
        let data = &mut pkg.workbook.sheets[0];
        let gone = [
            (1, "Macro1!A1+1", "#REF!+1"),
            (2, "SUM(macro1!A1:B2)*2", "SUM(#REF!)*2"),
            (3, "SUM(Macro1!A:A)", "SUM(#REF!)"),
            (4, "SUM(Report!A1,Dialog1!$B$2)", "SUM(Report!A1,#REF!)"),
            // A 3-D span with a removed end sheet.
            (5, "SUM(Data:Macro1!A1)", "SUM(#REF!)"),
        ];
        for (r, src, _) in gone {
            data.set_cell(r, 0, Cell::formula(src));
        }
        let kept = [
            (1, "Report!A1*2"),
            // Macro1 lies between Data and Report: it just leaves the span.
            (2, "SUM(Data:Report!A1)"),
            (3, "IF(A1=1,\"Macro1!A1\",B1)"),
            (4, "XMacro1!A1"),
        ];
        for (r, src) in kept {
            data.set_cell(r, 1, Cell::formula(src));
        }
        let mut array = Cell::formula("Macro1!A1:A2");
        array.f_attrs = Some(" t=\"array\" ref=\"C1:C2\"".into());
        array.spill = Some((2, 1));
        data.set_cell(0, 2, array);
        // The block's other cell, holding a value computed from Macro1.
        data.set_cell(1, 2, Cell::number(7.0));
        // Error handling sees the #REF!, so the cached value is its answer.
        let handled = [
            (
                1,
                "IFERROR(Macro1!A1,0)",
                "IFERROR(#REF!,0)",
                CellValue::Number(0.0),
            ),
            (
                2,
                "ISERROR(Macro1!A1)",
                "ISERROR(#REF!)",
                CellValue::Bool(true),
            ),
            (
                3,
                "Report!A1*0+Macro1!A1",
                "Report!A1*0+#REF!",
                CellValue::Error("#REF!".into()),
            ),
        ];
        for (r, src, _, _) in &handled {
            data.set_cell(*r, 3, Cell::formula(src));
        }
        let formulas = |pkg: &SheetPackage| -> Vec<(u32, u32, Option<String>)> {
            let mut out: Vec<_> = pkg.workbook.sheets[0]
                .cells
                .iter()
                .map(|(&(r, c), cell)| (r, c, cell.formula.clone()))
                .collect();
            out.sort();
            out
        };
        let before = (formulas(&pkg), pkg.workbook.sheets.len());

        for kind in [SpreadsheetKind::Workbook, SpreadsheetKind::Template] {
            let out = roundtrip(&pkg, kind);
            let data = &out.workbook.sheets[0];
            for (r, _, want) in gone {
                let cell = data.cell(r, 0).unwrap();
                assert_eq!(cell.formula.as_deref(), Some(want), "row {r}");
                assert_eq!(cell.value, CellValue::Error("#REF!".into()), "row {r}");
            }
            for (r, src) in kept {
                assert_eq!(data.cell(r, 1).unwrap().formula.as_deref(), Some(src));
            }
            let array = data.cell(0, 2).unwrap();
            assert_eq!(array.formula.as_deref(), Some("#REF!"));
            assert!(array.f_attrs.as_deref().is_some_and(is_array_f));
            assert_eq!(array.value, CellValue::Error("#REF!".into()));
            assert_eq!(
                data.cell(1, 2).unwrap().value,
                CellValue::Error("#REF!".into()),
                "the array's block follows its anchor"
            );
            for (r, _, want, value) in &handled {
                let cell = data.cell(*r, 3).unwrap();
                assert_eq!(cell.formula.as_deref(), Some(*want), "row {r}");
                assert_eq!(&cell.value, value, "row {r}");
            }
        }
        assert_eq!(
            (formulas(&pkg), pkg.workbook.sheets.len()),
            before,
            "the open workbook keeps its formulas"
        );

        // A quoted name is the same sheet; a macro-enabled save keeps it all.
        let mut pkg = xlm_book(&[("Data", "work"), ("Macro 1", "macro"), ("Report", "work")]);
        pkg.workbook.sheets[0].set_cell(1, 0, Cell::formula("'Macro 1'!A1+1"));
        let out = roundtrip(&pkg, SpreadsheetKind::Workbook);
        let cell = out.workbook.sheets[0].cell(1, 0).unwrap();
        assert_eq!(cell.formula.as_deref(), Some("#REF!+1"));
        let out = roundtrip(&pkg, SpreadsheetKind::MacroWorkbook);
        let cell = out.workbook.sheets[0].cell(1, 0).unwrap();
        assert_eq!(cell.formula.as_deref(), Some("'Macro 1'!A1+1"));
    }

    #[test]
    fn macro_features_names_everything_a_macro_free_file_loses() {
        assert!(new_xlsx().macro_features().is_empty());
        assert_eq!(xlsm().macro_features(), ["VB project"]);
        assert_eq!(
            mixed_xlm_book().macro_features(),
            [
                "Excel 4.0 macro sheets",
                "Excel 4.0 function stored in defined names"
            ]
        );
    }
}

/// References kept outside the cells (defined names, print area and titles,
/// page breaks) follow row/column inserts and deletes into the saved
/// file (#611).
#[cfg(test)]
mod print_setup_tests {
    use super::*;
    use crate::edit::{delete_cols, delete_rows, insert_cols, insert_rows, rename_sheet};

    const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

    /// A workbook of `sheets` (name, worksheet body inside `<worksheet>`; None
    /// leaves the sheet's part out of the package) with `names` as the
    /// `<definedNames>` content.
    pub(super) fn book(names: &str, sheets: &[(&str, Option<&str>)]) -> Vec<u8> {
        let mut sheet_els = String::new();
        let mut rels = String::new();
        let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
        for (i, (name, body)) in sheets.iter().enumerate() {
            let n = i + 1;
            sheet_els.push_str(&format!(
                r#"<sheet name="{name}" sheetId="{n}" r:id="rId{n}"/>"#
            ));
            rels.push_str(&format!(
                r#"<Relationship Id="rId{n}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{n}.xml"/>"#
            ));
            if let Some(body) = body {
                parts.push((
                    format!("xl/worksheets/sheet{n}.xml"),
                    format!(r#"<?xml version="1.0"?><worksheet xmlns="{NS}">{body}</worksheet>"#)
                        .into_bytes(),
                ));
            }
        }
        let workbook = format!(
            r#"<?xml version="1.0"?><workbook xmlns="{NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>{sheet_els}</sheets><definedNames>{names}</definedNames></workbook>"#
        );
        let wb_rels = format!(
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#
        );
        let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;
        parts.extend([
            ("[Content_Types].xml".into(), content_types.into()),
            ("_rels/.rels".into(), root_rels.into()),
            ("xl/workbook.xml".into(), workbook.into_bytes()),
            ("xl/_rels/workbook.xml.rels".into(), wb_rels.into_bytes()),
        ]);
        write_zip(&parts)
    }

    const PRINT_NAMES: &str = concat!(
        r#"<definedName name="_xlnm.Print_Area" localSheetId="0">Report!$A$1:$D$20</definedName>"#,
        r#"<definedName name="_xlnm.Print_Titles" localSheetId="0">Report!$A:$A,Report!$1:$2</definedName>"#,
    );

    fn report(names: &str, after_data: &str) -> SheetPackage {
        let body = format!(
            r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>{after_data}"#
        );
        load_xlsx(&book(names, &[("Report", Some(&body))])).expect("fixture loads")
    }

    /// Save, reload, and return the reloaded package with the text of `part`.
    fn saved(pkg: &SheetPackage, part: &str) -> (SheetPackage, String) {
        let re = load_xlsx(&save_xlsx(pkg)).expect("saved file reloads");
        let xml = String::from_utf8_lossy(re.part(part).expect("part present")).into_owned();
        (re, xml)
    }

    fn row_ids(sheet: &Sheet) -> Vec<u32> {
        sheet.row_breaks.iter().map(|b| b.id).collect()
    }

    fn col_ids(sheet: &Sheet) -> Vec<u32> {
        sheet.col_breaks.iter().map(|b| b.id).collect()
    }

    #[test]
    fn issue_repro_print_area_titles_and_break_follow_row_and_col_insert() {
        let mut pkg = report(
            PRINT_NAMES,
            r#"<rowBreaks count="1" manualBreakCount="1"><brk id="13" max="16383" man="1"/></rowBreaks>"#,
        );
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        insert_cols(&mut pkg.workbook, 0, 0, 1);
        let (re, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(r#"<definedName name="_xlnm.Print_Area" localSheetId="0">Report!$B$2:$E$21</definedName>"#),
            "{wb}"
        );
        assert!(
            wb.contains(r#"<definedName name="_xlnm.Print_Titles" localSheetId="0">Report!$B:$B,Report!$2:$3</definedName>"#),
            "{wb}"
        );
        let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(
            ws.contains(r#"<rowBreaks count="1" manualBreakCount="1"><brk id="14" max="16383" man="1"/></rowBreaks>"#),
            "{ws}"
        );
        assert_eq!(row_ids(&re.workbook.sheets[0]), vec![14]);
    }

    #[test]
    fn a_break_moves_when_rows_or_cols_go_in_at_or_above_it() {
        let breaks = concat!(
            r#"<rowBreaks count="1" manualBreakCount="1"><brk id="13" max="16383" man="1"/></rowBreaks>"#,
            r#"<colBreaks count="1" manualBreakCount="1"><brk id="13" max="1048575" man="1"/></colBreaks>"#,
        );
        for (at, want) in [(0, 16), (12, 16), (13, 16), (14, 13), (40, 13)] {
            let mut pkg = report("", breaks);
            insert_rows(&mut pkg.workbook, 0, at, 3);
            assert_eq!(
                row_ids(&pkg.workbook.sheets[0]),
                vec![want],
                "row insert at {at}"
            );
            assert_eq!(col_ids(&pkg.workbook.sheets[0]), vec![13], "cols untouched");
            let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
            assert!(
                ws.contains(&format!(r#"<brk id="{want}" max="16383" man="1"/>"#)),
                "{ws}"
            );

            let mut pkg = report("", breaks);
            insert_cols(&mut pkg.workbook, 0, at, 3);
            assert_eq!(
                col_ids(&pkg.workbook.sheets[0]),
                vec![want],
                "col insert at {at}"
            );
            assert_eq!(row_ids(&pkg.workbook.sheets[0]), vec![13], "rows untouched");
            let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
            assert!(
                ws.contains(&format!(r#"<brk id="{want}" max="1048575" man="1"/>"#)),
                "{ws}"
            );
        }
    }

    #[test]
    fn deleting_a_breaks_row_removes_it_and_recounts() {
        // An automatic break (no `man`) at 5, manual ones at 13 and 20.
        let mut pkg = report(
            "",
            concat!(
                r#"<rowBreaks count="3" manualBreakCount="2"><brk id="5" max="16383"/>"#,
                r#"<brk id="13" max="16383" man="1"/><brk id="20" max="16383" man="1"/></rowBreaks>"#,
            ),
        );
        delete_rows(&mut pkg.workbook, 0, 13, 2); // starts exactly at the break
        assert_eq!(row_ids(&pkg.workbook.sheets[0]), vec![5, 18]);
        let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(
            ws.contains(concat!(
                r#"<rowBreaks count="2" manualBreakCount="1"><brk id="5" max="16383"/>"#,
                r#"<brk id="18" max="16383" man="1"/></rowBreaks>"#,
            )),
            "{ws}"
        );

        // The last break going takes the element with it.
        let mut pkg = report(
            "",
            r#"<rowBreaks count="1" manualBreakCount="1"><brk id="13" max="16383" man="1"/></rowBreaks><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>"#,
        );
        delete_rows(&mut pkg.workbook, 0, 10, 5);
        let (re, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(!ws.contains("rowBreaks") && !ws.contains("<brk"), "{ws}");
        assert!(ws.contains("<pageMargins "), "{ws}");
        assert!(re.workbook.sheets[0].row_breaks.is_empty());
    }

    #[test]
    fn a_user_name_follows_an_insert_and_still_resolves() {
        let body = r#"<sheetData><row r="1"><c r="B1"><f>Rate*2</f><v>14</v></c></row><row r="5"><c r="A5"><v>7</v></c></row></sheetData>"#;
        let mut pkg = load_xlsx(&book(
            r#"<definedName name="Rate">Report!$A$5</definedName>"#,
            &[("Report", Some(body))],
        ))
        .unwrap();
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        pkg.workbook.sheets[0].set_cell(5, 0, crate::sheet::Cell::number(9.0)); // A6
        let (mut re, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(r#"<definedName name="Rate">Report!$A$6</definedName>"#),
            "{wb}"
        );
        let mut eng = crate::engine::Engine::new(&re.workbook);
        eng.recalc_all(&mut re.workbook);
        let b2 = re.workbook.sheets[0].cell(1, 1).unwrap();
        assert_eq!(b2.formula.as_deref(), Some("Rate*2"));
        assert_eq!(b2.value, CellValue::Number(18.0));
    }

    /// The `<definedNames>…</definedNames>` element of a workbook.xml.
    fn names_el(wb: &str) -> &str {
        let s = wb.find("<definedNames>").unwrap();
        let e = wb.find("</definedNames>").unwrap() + "</definedNames>".len();
        &wb[s..e]
    }

    #[test]
    fn untouched_names_and_breaks_keep_their_bytes() {
        let names = concat!(
            r#"<definedName name="_xlnm.Print_Area" localSheetId="0">'Report'!$A$1:$D$20</definedName>"#,
            r#"<definedName name="_xlnm._FilterDatabase" localSheetId="0" hidden="1">Report!$A$1:$A$20</definedName>"#,
            r#"<definedName name="Far">'Other'!$A$1</definedName>"#,
            r#"<definedName name="Mixed">Other!$A$1 + 0</definedName>"#,
        );
        let breaks = r#"<rowBreaks count="1" manualBreakCount="1"><brk id="13" max="16383" man="1" /></rowBreaks>"#;
        let data = r#"<sheetData/>"#;
        let body = format!("{data}{breaks}");
        let file = book(names, &[("Report", Some(&body)), ("Other", Some(data))]);

        // No structural edit: everything byte-for-byte.
        let pkg = load_xlsx(&file).unwrap();
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert_eq!(
            names_el(&wb),
            format!("<definedNames>{names}</definedNames>")
        );
        let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(ws.contains(breaks), "{ws}");

        // An insert on Other's columns reaches none of Report's names or
        // breaks, and the names on Other lie to its left.
        let mut pkg = load_xlsx(&file).unwrap();
        insert_cols(&mut pkg.workbook, 1, 5, 1);
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert_eq!(
            names_el(&wb),
            format!("<definedNames>{names}</definedNames>")
        );
        let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(ws.contains(breaks), "{ws}");

        // An insert on Report moves its print area and _FilterDatabase (#731)
        // (and only those); the quoted name on Other keeps its exact text.
        let mut pkg = load_xlsx(&file).unwrap();
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(r#"localSheetId="0">Report!$A$2:$D$21</definedName>"#),
            "{wb}"
        );
        assert!(wb.contains(r#"<definedName name="_xlnm._FilterDatabase" localSheetId="0" hidden="1">Report!$A$2:$A$21</definedName>"#), "{wb}");
        assert!(
            wb.contains(r#"<definedName name="Far">'Other'!$A$1</definedName>"#),
            "{wb}"
        );
        assert!(
            wb.contains(r#"<definedName name="Mixed">Other!$A$1 + 0</definedName>"#),
            "{wb}"
        );
    }

    #[test]
    fn a_wholly_deleted_area_reads_ref_error_as_excel_writes_it() {
        let mut pkg = report(PRINT_NAMES, "");
        delete_rows(&mut pkg.workbook, 0, 0, 20);
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(r#"<definedName name="_xlnm.Print_Area" localSheetId="0">Report!#REF!</definedName>"#),
            "{wb}"
        );
        // The titles' rows went too; their column area stays as it was.
        assert!(
            wb.contains(r#"<definedName name="_xlnm.Print_Titles" localSheetId="0">Report!$A:$A,Report!#REF!</definedName>"#),
            "{wb}"
        );
    }

    #[test]
    fn scoped_names_are_left_alone_when_sheet_ids_do_not_line_up() {
        // The second sheet's part is missing, so the model has two sheets and
        // localSheetId 2 is the model's sheet 1.
        let data = r#"<sheetData/>"#;
        let names = concat!(
            r#"<definedName name="Local" localSheetId="2">Third!$A$1</definedName>"#,
            r#"<definedName name="Global">Third!$A$1</definedName>"#,
        );
        let mut pkg = load_xlsx(&book(
            names,
            &[
                ("Report", Some(data)),
                ("Gone", None),
                ("Third", Some(data)),
            ],
        ))
        .unwrap();
        assert_eq!(pkg.workbook.sheets.len(), 2);
        assert_eq!(pkg.workbook.defined_name("Local", 1), Some("Third!$A$1"));
        insert_rows(&mut pkg.workbook, 1, 0, 1);
        assert_eq!(pkg.workbook.defined_name("Local", 1), Some("Third!$A$2"));
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(r#"<definedName name="Local" localSheetId="2">Third!$A$1</definedName>"#),
            "{wb}"
        );
        assert!(
            wb.contains(r#"<definedName name="Global">Third!$A$2</definedName>"#),
            "{wb}"
        );
    }

    #[test]
    fn a_rewrite_keeps_the_other_attributes_of_names_and_breaks() {
        let names = r#"<definedName name="Rate" comment="a &amp; b" hidden="1" function="0">Report!$A$5</definedName>"#;
        let mut pkg = report(
            names,
            r#"<rowBreaks count="1" manualBreakCount="1"><brk id="13" min="2" max="40" man="1" pt="1"/></rowBreaks>"#,
        );
        insert_rows(&mut pkg.workbook, 0, 0, 2);
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(r#"<definedName name="Rate" comment="a &amp; b" hidden="1" function="0">Report!$A$7</definedName>"#),
            "{wb}"
        );
        let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(
            ws.contains(r#"<brk id="15" min="2" max="40" man="1" pt="1"/>"#),
            "{ws}"
        );
    }

    #[test]
    fn a_sheet_rename_reaches_the_saved_names() {
        let mut pkg = report(PRINT_NAMES, "");
        rename_sheet(&mut pkg.workbook, 0, "Q1 Report");
        let (re, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(r#"localSheetId="0">'Q1 Report'!$A$1:$D$20</definedName>"#),
            "{wb}"
        );
        assert!(
            wb.contains(r#"localSheetId="0">'Q1 Report'!$A:$A,'Q1 Report'!$1:$2</definedName>"#),
            "{wb}"
        );
        assert_eq!(re.workbook.sheets[0].name, "Q1 Report");
    }

    #[test]
    fn escaped_name_text_is_compared_decoded() {
        // The text decodes to `"R&D"`, which `esc_text` would write back as
        // `"R&amp;D"`: only a decoded compare sees that nothing changed.
        let names = r#"<definedName name="Label">&quot;R&amp;D&quot;</definedName>"#;
        let mut pkg = report(names, "");
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(wb.contains(names), "{wb}");
    }

    #[test]
    fn a_custom_views_breaks_are_its_own_and_stay_put() {
        // `<customSheetViews>` precedes the sheet's own breaks, and each view
        // keeps breaks under the same element names.
        let view = concat!(
            r#"<customSheetViews><customSheetView guid="{00000000-0000-0000-0000-000000000001}">"#,
            r#"<rowBreaks count="1" manualBreakCount="1"><brk id="4" max="16383" man="1"/></rowBreaks>"#,
            r#"<colBreaks count="1" manualBreakCount="1"><brk id="2" max="1048575" man="1"/></colBreaks>"#,
            r#"</customSheetView></customSheetViews>"#,
        );
        let own = r#"<rowBreaks count="1" manualBreakCount="1"><brk id="13" max="16383" man="1"/></rowBreaks>"#;
        let body =
            format!(r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>{view}{own}"#);
        let file = book("", &[("Report", Some(&body))]);

        let pkg = load_xlsx(&file).unwrap();
        assert_eq!(row_ids(&pkg.workbook.sheets[0]), vec![13]);
        assert!(pkg.workbook.sheets[0].col_breaks.is_empty());
        let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(ws.contains(&format!("{view}{own}")), "{ws}");

        let mut pkg = load_xlsx(&file).unwrap();
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        insert_cols(&mut pkg.workbook, 0, 0, 1);
        let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(
            ws.contains(&format!(
                r#"{view}<rowBreaks count="1" manualBreakCount="1"><brk id="14" max="16383" man="1"/></rowBreaks>"#
            )),
            "{ws}"
        );
    }

    const DATA: &str = r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>"#;
    const FROZEN: &str =
        r#"<pane xSplit="1" ySplit="2" topLeftCell="B3" activePane="bottomRight" state="frozen"/>"#;

    /// Load `body` as the one sheet, check its freeze, and check a plain save
    /// leaves its `<sheetViews>` and `<customSheetViews>` as they were.
    fn freeze_of_and_kept(body: &str, views: &[&str]) -> (u32, u32) {
        let pkg = load_xlsx(&book("", &[("Report", Some(body))])).unwrap();
        let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        for v in views {
            assert!(ws.contains(v), "{ws}");
        }
        pkg.workbook.sheets[0].freeze
    }

    #[test]
    fn a_custom_views_pane_is_not_the_sheets_freeze() {
        // The sheet's own view is unfrozen; a custom view freezes two rows
        // and a column. Read as the sheet's, a save would freeze the sheet.
        let own = r#"<sheetViews><sheetView workbookViewId="0"/></sheetViews>"#;
        let custom = format!(
            r#"<customSheetViews><customSheetView guid="{{00000000-0000-0000-0000-000000000001}}">{FROZEN}</customSheetView></customSheetViews>"#
        );
        let body = format!("{own}{DATA}{custom}");
        assert_eq!(freeze_of_and_kept(&body, &[own, &custom]), (0, 0));
    }

    #[test]
    fn a_second_sheet_views_pane_is_not_the_sheets_freeze() {
        // A second `<sheetView>` is another workbook window's view.
        let views = format!(
            r#"<sheetViews><sheetView workbookViewId="0"></sheetView><sheetView workbookViewId="1">{FROZEN}</sheetView></sheetViews>"#
        );
        let body = format!("{views}{DATA}");
        assert_eq!(freeze_of_and_kept(&body, &[&views]), (0, 0));

        // The first view frozen, the second split differently: the first's.
        let views = format!(
            r#"<sheetViews><sheetView workbookViewId="0">{FROZEN}</sheetView><sheetView workbookViewId="1"><pane ySplit="5" topLeftCell="A6" activePane="bottomLeft" state="frozen"/></sheetView></sheetViews>"#
        );
        let body = format!("{views}{DATA}");
        assert_eq!(freeze_of_and_kept(&body, &[&views]), (2, 1));
    }

    #[test]
    fn a_self_closing_first_sheet_view_does_not_take_the_second_views_pane() {
        let views = format!(
            r#"<sheetViews><sheetView workbookViewId="0"/><sheetView workbookViewId="1">{FROZEN}</sheetView></sheetViews>"#
        );
        let body = format!("{views}{DATA}");
        assert_eq!(freeze_of_and_kept(&body, &[&views]), (0, 0));
    }

    #[test]
    fn a_gt_inside_a_quoted_name_attribute_is_not_the_tag_end() {
        let names = concat!(
            r#"<definedName name="Rate" comment="rate > 0">Report!$A$5</definedName>"#,
            r#"<definedName name="Top" comment='a "quoted" > b'>Report!$A$1</definedName>"#,
        );
        let mut pkg = report(names, "");
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (re, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(r#"<definedName name="Rate" comment="rate > 0">Report!$A$6</definedName>"#),
            "{wb}"
        );
        assert!(
            wb.contains(
                r#"<definedName name="Top" comment='a "quoted" > b'>Report!$A$2</definedName>"#
            ),
            "{wb}"
        );
        assert_eq!(re.workbook.defined_name("Rate", 0), Some("Report!$A$6"));

        // And with no edit the file keeps its bytes.
        let pkg = report(names, "");
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(wb.contains(names), "{wb}");
    }

    #[test]
    fn names_sharing_a_key_in_the_model_are_not_cross_written() {
        // `Total` scoped to the missing second sheet loads as global, next to
        // the real global `Total`: two model entries, one key.
        let data = r#"<sheetData/>"#;
        let names = concat!(
            r#"<definedName name="Total" localSheetId="1">Report!$A$1</definedName>"#,
            r#"<definedName name="Total">Report!$B$9</definedName>"#,
        );
        let file = book(names, &[("Report", Some(data)), ("Gone", None)]);
        let pkg = load_xlsx(&file).unwrap();
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(wb.contains(names), "{wb}");

        // Same when a writer repeated a global name outright.
        let names = concat!(
            r#"<definedName name="Dup">Report!$A$1</definedName>"#,
            r#"<definedName name="Dup">Report!$B$9</definedName>"#,
        );
        let mut pkg = report(names, "");
        insert_rows(&mut pkg.workbook, 0, 20, 1);
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(wb.contains(names), "{wb}");
    }

    #[test]
    fn a_union_with_a_deleted_area_still_moves_its_other_areas() {
        let mut pkg = report(PRINT_NAMES, "");
        delete_rows(&mut pkg.workbook, 0, 0, 20);
        insert_cols(&mut pkg.workbook, 0, 0, 1);
        let (re, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(r#"<definedName name="_xlnm.Print_Titles" localSheetId="0">Report!$B:$B,Report!#REF!</definedName>"#),
            "{wb}"
        );
        // Also once it has been through a save and reload.
        let mut re = re;
        insert_cols(&mut re.workbook, 0, 0, 1);
        let (_, wb) = saved(&re, "xl/workbook.xml");
        assert!(
            wb.contains(r#"localSheetId="0">Report!$C:$C,Report!#REF!</definedName>"#),
            "{wb}"
        );
    }

    #[test]
    fn cdata_and_comments_in_names_are_read_as_the_loader_reads_them() {
        let names = concat!(
            r#"<definedName name="Cdata"><![CDATA[Report!$A$30]]></definedName>"#,
            r#"<definedName name="Noted">Report!$A$31<!-- not </definedName> yet --></definedName>"#,
            r#"<definedName name="Moves"><![CDATA[Report!$A$5]]></definedName>"#,
        );
        let file = || {
            let body = r#"<sheetData/>"#;
            load_xlsx(&book(names, &[("Report", Some(body))])).unwrap()
        };
        // Plain save, and an edit below them: byte-identical.
        let (_, wb) = saved(&file(), "xl/workbook.xml");
        assert!(wb.contains(names), "{wb}");
        let mut pkg = file();
        insert_rows(&mut pkg.workbook, 0, 40, 1);
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(wb.contains(names), "{wb}");

        // An edit that moves them rewrites the whole content, well-formed.
        let mut pkg = file();
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (re, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(concat!(
                r#"<definedName name="Cdata">Report!$A$31</definedName>"#,
                r#"<definedName name="Noted">Report!$A$32</definedName>"#,
                r#"<definedName name="Moves">Report!$A$6</definedName>"#,
            )),
            "{wb}"
        );
        assert_eq!(re.workbook.defined_name("Noted", 0), Some("Report!$A$32"));
    }

    #[test]
    fn a_comment_inside_a_breaks_element_does_not_end_it() {
        let breaks = concat!(
            r#"<rowBreaks count="2" manualBreakCount="2"><!-- </rowBreaks> -->"#,
            r#"<brk id="5" max="16383" man="1"/><brk id="13" max="16383" man="1"/></rowBreaks>"#,
        );
        let pkg = report("", breaks);
        assert_eq!(row_ids(&pkg.workbook.sheets[0]), vec![5, 13]);
        let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(ws.contains(breaks), "{ws}");

        let mut pkg = report("", breaks);
        insert_rows(&mut pkg.workbook, 0, 10, 1);
        let (re, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(
            ws.contains(concat!(
                r#"<rowBreaks count="2" manualBreakCount="2"><brk id="5" max="16383" man="1"/>"#,
                r#"<brk id="14" max="16383" man="1"/></rowBreaks></worksheet>"#,
            )),
            "{ws}"
        );
        assert_eq!(row_ids(&re.workbook.sheets[0]), vec![5, 14]);
    }

    #[test]
    fn a_rename_reaches_deleted_areas_in_the_saved_names() {
        let mut pkg = report(PRINT_NAMES, "");
        delete_rows(&mut pkg.workbook, 0, 0, 20);
        rename_sheet(&mut pkg.workbook, 0, "Q1");
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(
                r#"<definedName name="_xlnm.Print_Area" localSheetId="0">Q1!#REF!</definedName>"#
            ),
            "{wb}"
        );
        assert!(
            wb.contains(r#"<definedName name="_xlnm.Print_Titles" localSheetId="0">Q1!$A:$A,Q1!#REF!</definedName>"#),
            "{wb}"
        );
    }

    #[test]
    fn non_ascii_text_around_names_and_breaks_is_scanned_safely() {
        // Element lookups step byte by byte; text such as `Ü` before the
        // element they look for must not panic them.
        let names = r#"<definedName name="Größe">'Übersicht'!$A$5</definedName>"#;
        let body = concat!(
            r#"<sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Ärger</t></is></c></row></sheetData>"#,
            r#"<rowBreaks count="1" manualBreakCount="1"><brk id="13" max="16383" man="1"/></rowBreaks>"#,
        );
        let mut pkg = load_xlsx(&book(names, &[("Übersicht", Some(body))])).unwrap();
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(r#"<definedName name="Größe">Übersicht!$A$6</definedName>"#),
            "{wb}"
        );
        let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(ws.contains(r#"<brk id="14" max="16383" man="1"/>"#), "{ws}");
    }

    // --- #731: the sheet autoFilter and _FilterDatabase move together ------

    const FILTER_DB: &str = r#"<definedName name="_xlnm._FilterDatabase" localSheetId="0" hidden="1">Report!$A$2:$C$9</definedName>"#;

    /// The text of `_FilterDatabase` in a saved workbook.xml.
    fn filter_db(wb: &str) -> &str {
        let start = wb
            .find(r#"name="_xlnm._FilterDatabase""#)
            .expect("name kept");
        let body = start + wb[start..].find('>').unwrap() + 1;
        &wb[body..body + wb[body..].find("</definedName>").unwrap()]
    }

    /// The saved worksheet and workbook.xml after `edit` on a Report sheet
    /// holding `after_data` after its data, with `names`.
    fn edited(names: &str, after_data: &str, edit: fn(&mut Workbook)) -> (String, String) {
        let mut pkg = report(names, after_data);
        edit(&mut pkg.workbook);
        let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        (ws, wb)
    }

    #[test]
    fn sheet_auto_filter_and_filter_database_follow_row_insert() {
        let mut pkg = report(FILTER_DB, r#"<autoFilter ref="A2:C9"/>"#);
        insert_rows(&mut pkg.workbook, 0, 0, 2);
        let (re, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
        assert!(ws.contains(r#"<autoFilter ref="A4:C11"/>"#), "{ws}");
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert_eq!(filter_db(&wb), "Report!$A$4:$C$11");
        let af = re.workbook.sheets[0].auto_filter.as_ref().unwrap();
        assert_eq!(af.range, (3, 0, 10, 2));
    }

    #[test]
    fn sheet_auto_filter_stretches_shrinks_and_goes_with_its_rows() {
        type Edit = fn(&mut Workbook);
        // (edit, autoFilter ref, name); None: the element is gone.
        let cases: [(Edit, Option<&str>, &str); 5] = [
            (
                |w| insert_rows(w, 0, 4, 1),
                Some("A2:C10"),
                "Report!$A$2:$C$10",
            ),
            (
                |w| insert_rows(w, 0, 1, 1),
                Some("A3:C10"),
                "Report!$A$3:$C$10",
            ),
            (
                |w| delete_rows(w, 0, 3, 2),
                Some("A2:C7"),
                "Report!$A$2:$C$7",
            ),
            (
                |w| delete_rows(w, 0, 0, 3),
                Some("A1:C6"),
                "Report!$A$1:$C$6",
            ),
            (|w| delete_rows(w, 0, 1, 8), None, "Report!#REF!"),
        ];
        for (i, (edit, want, name)) in cases.into_iter().enumerate() {
            let (ws, wb) = edited(FILTER_DB, r#"<autoFilter ref="A2:C9"/>"#, edit);
            match want {
                Some(r) => assert!(
                    ws.contains(&format!(r#"<autoFilter ref="{r}"/>"#)),
                    "case {i}: {ws}"
                ),
                None => assert!(!ws.contains("autoFilter"), "case {i}: {ws}"),
            }
            assert_eq!(filter_db(&wb), name, "case {i}");
        }
    }

    #[test]
    fn sheet_auto_filter_columns_renumber_and_drop_on_col_edits() {
        // B2:D9: a button-only column on B, a value filter on D, and the sort
        // Excel remembers for it.
        let names = r#"<definedName name="_xlnm._FilterDatabase" localSheetId="0" hidden="1">Report!$B$2:$D$9</definedName>"#;
        let filter = concat!(
            r#"<autoFilter ref="B2:D9" xr:uid="{AF}" xmlns:xr="urn:xr">"#,
            r#"<filterColumn colId="0" hiddenButton="1"/>"#,
            r#"<filterColumn colId="2"><filters blank="1"><filter val="East"/></filters></filterColumn>"#,
            r#"<sortState ref="B3:D9"><sortCondition ref="D3:D9"/></sortState>"#,
            r#"</autoFilter>"#,
        );

        // Left of it: the ref moves, the columns keep their ids and bytes,
        // the sort (naming the old cells) goes.
        let (ws, wb) = edited(names, filter, |w| insert_cols(w, 0, 0, 1));
        assert!(
            ws.contains(concat!(
                r#"<autoFilter ref="C2:E9" xr:uid="{AF}" xmlns:xr="urn:xr">"#,
                r#"<filterColumn colId="0" hiddenButton="1"/>"#,
                r#"<filterColumn colId="2"><filters blank="1"><filter val="East"/></filters></filterColumn>"#,
                r#"</autoFilter>"#,
            )),
            "{ws}"
        );
        assert_eq!(filter_db(&wb), "Report!$C$2:$E$9");

        // Inside it: the range stretches, D's filter is now colId 3.
        let (ws, wb) = edited(names, filter, |w| insert_cols(w, 0, 2, 1));
        assert!(
            ws.contains(concat!(
                r#"<autoFilter ref="B2:E9" xr:uid="{AF}" xmlns:xr="urn:xr">"#,
                r#"<filterColumn colId="0" hiddenButton="1"/>"#,
                r#"<filterColumn colId="3"><filters blank="1"><filter val="East"/></filters></filterColumn>"#,
                r#"</autoFilter>"#,
            )),
            "{ws}"
        );
        assert_eq!(filter_db(&wb), "Report!$B$2:$E$9");

        // Deleting B takes its filterColumn; D (now C) is colId 1.
        let (ws, wb) = edited(names, filter, |w| delete_cols(w, 0, 1, 1));
        assert!(
            ws.contains(concat!(
                r#"<autoFilter ref="B2:C9" xr:uid="{AF}" xmlns:xr="urn:xr">"#,
                r#"<filterColumn colId="1"><filters blank="1"><filter val="East"/></filters></filterColumn>"#,
                r#"</autoFilter>"#,
            )),
            "{ws}"
        );
        assert_eq!(filter_db(&wb), "Report!$B$2:$C$9");

        // Right of it: nothing to rewrite, the sort stays.
        let (ws, wb) = edited(names, filter, |w| delete_cols(w, 0, 6, 2));
        assert!(ws.contains(filter), "{ws}");
        assert_eq!(filter_db(&wb), "Report!$B$2:$D$9");
    }

    #[test]
    fn untouched_auto_filter_keeps_its_bytes() {
        let filter = concat!(
            r#"<autoFilter ref="A2:C9" ><filterColumn colId="1" ><customFilters>"#,
            r#"<customFilter operator="greaterThan" val="5"/></customFilters></filterColumn>"#,
            r#"<sortState ref="A3:C9"><sortCondition ref="B3:B9"/></sortState></autoFilter>"#,
        );
        let body =
            format!(r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>{filter}"#);
        let file = book(
            FILTER_DB,
            &[("Report", Some(&body)), ("Other", Some("<sheetData/>"))],
        );
        let edits: [fn(&mut Workbook); 4] = [
            |_| {},
            |w| insert_rows(w, 1, 0, 3),  // another sheet
            |w| insert_rows(w, 0, 20, 3), // below it
            |w| delete_cols(w, 0, 5, 1),  // right of it
        ];
        for (i, edit) in edits.into_iter().enumerate() {
            let mut pkg = load_xlsx(&file).unwrap();
            edit(&mut pkg.workbook);
            let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
            assert!(ws.contains(filter), "case {i}: {ws}");
            let (_, wb) = saved(&pkg, "xl/workbook.xml");
            assert_eq!(
                names_el(&wb),
                format!("<definedNames>{FILTER_DB}</definedNames>"),
                "case {i}"
            );
        }
    }

    const VIEWS: &str = r#"<customSheetViews><customSheetView guid="{V}"><autoFilter ref="A2:C9"><filterColumn colId="2"/></autoFilter></customSheetView></customSheetViews>"#;

    #[test]
    fn custom_view_auto_filter_is_left_alone() {
        // A sheet whose only autoFilter is a custom view's: nothing moves.
        let (ws, _) = edited(FILTER_DB, VIEWS, |w| insert_cols(w, 0, 0, 1));
        assert!(ws.contains(VIEWS), "{ws}");
        // Beside a top-level one: that one moves, the view's doesn't.
        let filter = format!(r#"<autoFilter ref="A2:C9"/>{VIEWS}"#);
        let (ws, wb) = edited(FILTER_DB, &filter, |w| insert_rows(w, 0, 0, 1));
        assert!(
            ws.contains(&format!(r#"<autoFilter ref="A3:C10"/>{VIEWS}"#)),
            "{ws}"
        );
        assert_eq!(filter_db(&wb), "Report!$A$3:$C$10");
    }

    #[test]
    fn an_auto_filter_whose_ref_is_not_a_range_is_left_verbatim() {
        let filter = r#"<autoFilter ref="bogus"><filterColumn colId="0"/></autoFilter>"#;
        let (ws, _) = edited("", filter, |w| delete_rows(w, 0, 0, 20));
        assert!(ws.contains(filter), "{ws}");
    }

    // --- #731: model names that have no element are written ---------------

    fn name(name: &str, scope: Option<usize>, formula: &str) -> DefinedName {
        DefinedName {
            name: name.to_string(),
            scope,
            formula: formula.to_string(),
        }
    }

    /// `pkg` with its workbook.xml's `<definedNames></definedNames>` replaced.
    fn without_names_el(mut pkg: SheetPackage, with: &str) -> SheetPackage {
        let part = pkg
            .parts
            .iter_mut()
            .find(|(n, _)| n == "xl/workbook.xml")
            .unwrap();
        let xml = String::from_utf8_lossy(&part.1).replace("<definedNames></definedNames>", with);
        part.1 = xml.into_bytes();
        pkg
    }

    #[test]
    fn a_model_name_without_an_element_is_written() {
        let mut pkg = report(PRINT_NAMES, "");
        let names = &mut pkg.workbook.defined_names;
        names.push(name("Rate", Some(0), "Report!$B$1"));
        names.push(name("Total", None, "SUM(Report!$A:$A)&\"<\""));
        names.push(name("Rate", Some(0), "Report!$C$1")); // a repeat
        let (re, wb) = saved(&pkg, "xl/workbook.xml");
        assert_eq!(
            names_el(&wb),
            format!(
                "<definedNames>{PRINT_NAMES}{}{}</definedNames>",
                r#"<definedName name="Rate" localSheetId="0">Report!$B$1</definedName>"#,
                r#"<definedName name="Total">SUM(Report!$A:$A)&amp;"&lt;"</definedName>"#,
            )
        );
        assert_eq!(re.workbook.defined_names.len(), 4);
        // Saved again: already there, so not written twice.
        let (_, again) = saved(&re, "xl/workbook.xml");
        assert_eq!(names_el(&again), names_el(&wb));
    }

    #[test]
    fn defined_names_save_in_file_spelling() {
        // #776: a definition written from the model gets its prefixes; one
        // loaded in file spelling is left as it was.
        let names = concat!(
            r#"<definedName name="Rate">Report!$A$5</definedName>"#,
            r#"<definedName name="Top">_xlfn.SEQUENCE(2)</definedName>"#,
        );
        let mut pkg = report(names, "");
        pkg.workbook
            .defined_names
            .iter_mut()
            .find(|d| d.name == "Rate")
            .unwrap()
            .formula = "LAMBDA(x,x*2)".into();
        pkg.workbook
            .defined_names
            .push(name("Seq", None, "SEQUENCE(3)"));
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(
                r#"<definedName name="Rate">_xlfn.LAMBDA(_xlpm.x,_xlpm.x*2)</definedName>"#
            ),
            "{wb}"
        );
        assert!(
            wb.contains(r#"<definedName name="Top">_xlfn.SEQUENCE(2)</definedName>"#),
            "{wb}"
        );
        assert!(
            wb.contains(r#"<definedName name="Seq">_xlfn.SEQUENCE(3)</definedName>"#),
            "{wb}"
        );
    }

    #[test]
    fn a_missing_defined_names_element_is_created_in_schema_order() {
        let mut pkg = without_names_el(report("", ""), r#"<calcPr calcId="191029"/>"#);
        let print_area = name("_xlnm.Print_Area", Some(0), "Report!$A$1:$B$2");
        pkg.workbook.defined_names.push(print_area.clone());
        let (re, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(concat!(
                r#"</sheets><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">"#,
                r#"Report!$A$1:$B$2</definedName></definedNames><calcPr calcId="191029"/>"#,
            )),
            "{wb}"
        );
        assert_eq!(re.workbook.defined_names, vec![print_area]);

        // A self-closing <definedNames/> is opened up.
        let mut pkg = without_names_el(report("", ""), "<definedNames/>");
        pkg.workbook
            .defined_names
            .push(name("Rate", None, "Report!$B$1"));
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert!(
            wb.contains(r#"<definedNames><definedName name="Rate">Report!$B$1</definedName></definedNames></workbook>"#),
            "{wb}"
        );
    }

    #[test]
    fn names_are_not_duplicated_when_scopes_do_not_line_up() {
        // Scoped to a sheet whose part is missing: the loader makes it global,
        // and the <sheet> elements no longer line up with the model.
        let names = concat!(
            r#"<definedName name="Local" localSheetId="1">Report!$A$1</definedName>"#,
            r#"<definedName name="Other" localSheetId="1">Report!$B$1</definedName>"#,
        );
        let file = book(names, &[("Report", Some("<sheetData/>")), ("Gone", None)]);
        let pkg = load_xlsx(&file).unwrap();
        assert_eq!(pkg.workbook.defined_names.len(), 2);
        assert!(pkg.workbook.defined_names.iter().all(|d| d.scope.is_none()));
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert_eq!(
            names_el(&wb),
            format!("<definedNames>{names}</definedNames>")
        );
    }

    #[test]
    fn an_out_of_range_local_sheet_id_is_not_duplicated() {
        let names = r#"<definedName name="Stray" localSheetId="5">Report!$A$1</definedName>"#;
        let pkg = report(names, "");
        assert_eq!(
            pkg.workbook.defined_names,
            vec![name("Stray", None, "Report!$A$1")]
        );
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert_eq!(
            names_el(&wb),
            format!("<definedNames>{names}</definedNames>")
        );
    }

    #[test]
    fn filter_database_is_not_appended_for_an_unfiltered_sheet() {
        let mut pkg = report(PRINT_NAMES, "");
        pkg.workbook
            .defined_names
            .push(name("_xlnm._FilterDatabase", Some(0), "Report!$A$1:$C$9"));
        let (_, wb) = saved(&pkg, "xl/workbook.xml");
        assert_eq!(
            names_el(&wb),
            format!("<definedNames>{PRINT_NAMES}</definedNames>")
        );
    }

    #[test]
    fn an_insert_pushing_the_auto_filter_off_the_sheet_drops_it_with_its_name() {
        type Edit = fn(&mut Workbook);
        // (filter ref, name, edit): the far edge is the sheet's last row/column.
        let cases: [(&str, &str, Edit); 2] = [
            ("A2:C1048576", "Report!$A$2:$C$1048576", |w| {
                insert_rows(w, 0, 0, 1)
            }),
            ("A2:XFD9", "Report!$A$2:$XFD$9", |w| insert_cols(w, 0, 0, 1)),
        ];
        for (r, name, edit) in cases {
            let names = format!(
                r#"<definedName name="_xlnm._FilterDatabase" localSheetId="0" hidden="1">{name}</definedName>"#
            );
            let (ws, wb) = edited(&names, &format!(r#"<autoFilter ref="{r}"/>"#), edit);
            assert!(!ws.contains("autoFilter"), "{r}: {ws}");
            assert_eq!(filter_db(&wb), "Report!#REF!", "{r}");
        }
    }

    #[test]
    fn a_stripped_macro_name_with_a_stray_scope_stays_out_of_a_macro_free_save() {
        let names = r#"<definedName name="Fn" xlm="1" localSheetId="5">Report!$A$1</definedName>"#;
        let pkg = report(names, "");
        let re = load_xlsx(&save_xlsx_as(&pkg, SpreadsheetKind::Workbook)).unwrap();
        let wb = String::from_utf8_lossy(re.part("xl/workbook.xml").unwrap()).into_owned();
        assert!(!wb.contains(r#"name="Fn""#), "{wb}");
        assert!(re.workbook.defined_names.is_empty());
    }
}

/// #597: every worksheet child the writer regenerates or inserts lands at its
/// `CT_Worksheet` position.
#[cfg(test)]
mod ct_worksheet_order_tests {
    use super::*;
    use crate::sheet::Cell;

    const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const SHEET: &str = "xl/worksheets/sheet1.xml";
    const ROWS: &str = r#"<sheetData><row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c><c r="B2"><v>2</v></c></row></sheetData>"#;
    const MARGINS: &str = r#"<pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>"#;

    /// A loaded workbook whose one worksheet is `<worksheet …>{body}</worksheet>`.
    fn loaded(body: &str) -> SheetPackage {
        let mut pkg = new_xlsx();
        pkg.set_part(
            SHEET,
            format!(
                r#"<?xml version="1.0"?><worksheet xmlns="{NS}" xmlns:r="{R}">{body}</worksheet>"#
            )
            .into_bytes(),
        );
        load_xlsx(&write_zip(&pkg.parts)).expect("load")
    }

    /// The worksheet as the next open sees it.
    fn saved_sheet(pkg: &SheetPackage) -> String {
        let re = load_xlsx(&save_xlsx(pkg)).expect("reload");
        String::from_utf8(re.part(SHEET).unwrap().to_vec()).unwrap()
    }

    /// Every top-level child the schema names comes in `CT_Worksheet` order.
    fn in_ct_worksheet_order(xml: &str) -> Result<(), String> {
        let walk = worksheet_children(xml);
        walk.close.ok_or("the walk can't finish")?;
        let ranked: Vec<(&str, usize)> = walk
            .children
            .iter()
            .filter_map(|c| ct_worksheet_rank(c.rank_as).map(|r| (c.rank_as, r)))
            .collect();
        if let Some(w) = ranked.windows(2).find(|w| w[0].1 > w[1].1) {
            return Err(format!("<{}> follows <{}>", w[1].0, w[0].0));
        }
        // Every CT_Worksheet child is maxOccurs=1 except conditionalFormatting.
        match ranked
            .windows(2)
            .find(|w| w[0].0 == w[1].0 && w[0].0 != "conditionalFormatting")
        {
            Some(w) => Err(format!("<{}> twice", w[0].0)),
            None => Ok(()),
        }
    }

    /// Every element the writer spells unprefixed or with the root's `x:`
    /// prefix resolves to SpreadsheetML, and every `r:id` to the rels
    /// namespace: nothing it wrote landed in no namespace or unbound.
    fn assert_names_bound(xml: &str) {
        let mut p = XmlParser::new(xml);
        loop {
            match p.next() {
                Event::Start => {
                    let lookup = |decl: &str| {
                        p.namespace_attrs()
                            .iter()
                            .rev()
                            .find(|a| a.name == decl)
                            .map(|a| a.value)
                    };
                    let decl = match p.name().split_once(':') {
                        Some(("x", _)) => "xmlns:x",
                        Some(_) => continue,
                        None => "xmlns",
                    };
                    assert_eq!(lookup(decl), Some(NS), "<{}> unbound: {xml}", p.name());
                    if p.attrs().iter().any(|a| a.name == "r:id") {
                        assert_eq!(lookup("xmlns:r"), Some(R), "<{}> r:id: {xml}", p.name());
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
    }

    fn count_local(xml: &str, name: &str) -> usize {
        let mut p = XmlParser::new(xml);
        let mut n = 0;
        loop {
            match p.next() {
                Event::Start if local(p.name()) == name => n += 1,
                Event::Eof => return n,
                _ => {}
            }
        }
    }

    /// A worksheet that binds SpreadsheetML only to `x:` (no default
    /// namespace) and declares no `r:`.
    fn loaded_prefixed(body: &str) -> SheetPackage {
        let mut pkg = new_xlsx();
        pkg.set_part(
            SHEET,
            format!(r#"<?xml version="1.0"?><x:worksheet xmlns:x="{NS}">{body}</x:worksheet>"#)
                .into_bytes(),
        );
        load_xlsx(&write_zip(&pkg.parts)).expect("load")
    }

    const PREFIXED: &str = r#"<x:dimension ref="A1:B2"/><x:sheetViews><x:sheetView workbookViewId="0"><x:pane ySplit="1" topLeftCell="A2" activePane="bottomLeft" state="frozen"/></x:sheetView></x:sheetViews><x:cols><x:col min="1" max="1" width="20" customWidth="1"/></x:cols><x:sheetData><x:row r="1"><x:c r="A1"><x:v>1</x:v></x:c><x:c r="B1"><x:v>2</x:v></x:c></x:row></x:sheetData><x:mergeCells count="1"><x:mergeCell ref="D1:E1"/></x:mergeCells><x:dataValidations count="1"><x:dataValidation type="whole" sqref="A1"><x:formula1>1</x:formula1></x:dataValidation></x:dataValidations><x:pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>"#;

    #[test]
    fn a_prefixed_worksheet_saves_without_duplicating_any_child() {
        let pkg = loaded_prefixed(PREFIXED);
        assert_eq!(pkg.workbook.sheets[0].merges.len(), 1);
        assert_eq!(pkg.workbook.sheets[0].freeze, (1, 0));
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert_names_bound(&ws);
        for name in [
            "sheetData",
            "cols",
            "sheetViews",
            "sheetView",
            "pane",
            "mergeCells",
        ] {
            assert_eq!(count_local(&ws, name), 1, "{name}: {ws}");
        }
        assert!(ws.contains("D1:E1"), "{ws}");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(re.workbook.sheets[0].merges, pkg.workbook.sheets[0].merges);
        assert_eq!(re.workbook.sheets[0].freeze, (1, 0));
        assert_eq!(re.workbook.sheets[0].col_defs.len(), 1);
    }

    #[test]
    fn edits_to_a_prefixed_worksheet_stay_in_its_namespace() {
        let mut pkg = loaded_prefixed(PREFIXED);
        let sheet = &mut pkg.workbook.sheets[0];
        sheet.freeze = (0, 1);
        sheet.merges.push((4, 0, 4, 1));
        sheet.set_protected(true);
        sheet.set_cell(1, 0, Cell::text("new"));
        let dxf = crate::sheet::Dxf {
            bold: Some(true),
            ..Default::default()
        };
        pkg.add_conditional_format(0, (0, 0, 1, 0), "greaterThan", "1", None, dxf);
        pkg.add_data_validation(0, (1, 1, 1, 1), "whole", "between", "1", Some("9"));
        pkg.add_table(0, (0, 0, 1, 1), true, "TableStyleMedium2")
            .expect("table");
        pkg.add_chart(0, (0, 3), (10, 8), &column_chart());
        pkg.set_comment(0, 0, 0, "A", "note");
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert_names_bound(&ws);
        for name in [
            "sheetViews",
            "pane",
            "mergeCells",
            "dataValidations",
            "tableParts",
        ] {
            assert_eq!(count_local(&ws, name), 1, "{name}: {ws}");
        }
        // The new rule joined the existing block.
        assert_eq!(count_local(&ws, "dataValidation"), 2, "{ws}");
        assert!(ws.contains(r#"<x:dataValidations count="2">"#), "{ws}");

        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let s = &re.workbook.sheets[0];
        assert_eq!(s.freeze, (0, 1));
        assert_eq!(s.merges.len(), 2);
        assert!(s.is_protected());
        assert_eq!(s.validations.len(), 2);
        assert_eq!(re.workbook.tables.len(), 1);
        assert_eq!(re.comments().len(), 1);
    }

    #[test]
    fn an_open_sheet_protection_element_is_replaced_whole() {
        let mut pkg = loaded(&format!(
            r#"{ROWS}<sheetProtection sheet="1" objects="1"></sheetProtection><protectedRanges><protectedRange sqref="A1" name="r"/></protectedRanges>"#
        ));
        assert!(pkg.workbook.sheets[0].is_protected());
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert_eq!(count_local(&ws, "sheetProtection"), 1, "{ws}");
        assert!(!ws.contains("</sheetProtection>"), "{ws}");
        assert!(
            ws.contains(
                r#"<protectedRanges><protectedRange sqref="A1" name="r"/></protectedRanges>"#
            ),
            "{ws}"
        );
        // Unprotecting removes it, and nothing else.
        pkg.workbook.sheets[0].set_protected(false);
        let ws = saved_sheet(&pkg);
        assert_eq!(count_local(&ws, "sheetProtection"), 0, "{ws}");
        assert!(ws.contains("<protectedRanges>"), "{ws}");
    }

    #[test]
    fn a_prefixed_sheet_protection_is_replaced_not_duplicated() {
        let pkg = loaded_prefixed(
            r#"<x:sheetData/><x:sheetProtection sheet="1"/><x:protectedRanges><x:protectedRange sqref="A1" name="r"/></x:protectedRanges>"#,
        );
        assert!(pkg.workbook.sheets[0].is_protected());
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert_names_bound(&ws);
        assert_eq!(count_local(&ws, "sheetProtection"), 1, "{ws}");
    }

    #[test]
    fn r_is_bound_on_the_element_when_the_root_uses_it_for_something_else() {
        let mut pkg = new_xlsx();
        pkg.set_part(
            SHEET,
            format!(
                r#"<?xml version="1.0"?><worksheet xmlns="{NS}" xmlns:r="urn:other">{ROWS}</worksheet>"#
            )
            .into_bytes(),
        );
        let mut pkg = load_xlsx(&write_zip(&pkg.parts)).unwrap();
        pkg.add_table(0, (0, 0, 1, 1), true, "TableStyleMedium2")
            .expect("table");
        let ws = saved_sheet(&pkg);
        assert!(
            ws.contains(r#"xmlns:r="urn:other""#),
            "the root is left alone: {ws}"
        );
        assert_names_bound(&ws);
    }

    fn assert_ct_worksheet_order(xml: &str) {
        if let Err(e) = in_ct_worksheet_order(xml) {
            panic!("{e}: {xml}");
        }
    }

    fn at(xml: &str, needle: &str) -> usize {
        xml.find(needle)
            .unwrap_or_else(|| panic!("{needle} missing: {xml}"))
    }

    #[test]
    fn a_plain_save_keeps_auto_filter_before_merge_cells() {
        // The #597 repro, as openpyxl writes it.
        let pkg = loaded(&format!(
            r#"<dimension ref="A1:B2"/>{ROWS}<autoFilter ref="A1:B10"/><mergeCells count="1"><mergeCell ref="D1:E1"/></mergeCells>{MARGINS}"#
        ));
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(at(&ws, "<autoFilter") < at(&ws, "<mergeCells"), "{ws}");
        assert!(ws.contains(r#"<mergeCell ref="D1:E1"/>"#), "{ws}");
    }

    #[test]
    fn merge_cells_follow_sort_state_consolidation_and_custom_views() {
        // The custom view carries its own <autoFilter>; only the top-level
        // children rank.
        let mut pkg = loaded(&format!(
            r#"{ROWS}<autoFilter ref="A1:B2"/><sortState ref="A2:B2"><sortCondition ref="A2:A2"/></sortState><dataConsolidate><dataRefs count="1"><dataRef ref="A1:B2"/></dataRefs></dataConsolidate><customSheetViews><customSheetView guid="{{00000000-0000-0000-0000-000000000001}}">{MARGINS}<autoFilter ref="A1:B2"/></customSheetView></customSheetViews>{MARGINS}"#
        ));
        pkg.workbook.sheets[0].merges.push((3, 0, 3, 1));
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(
            at(&ws, "</customSheetViews>") < at(&ws, "<mergeCells"),
            "{ws}"
        );
        assert!(
            at(&ws, "<mergeCells") < ws.rfind("<pageMargins").unwrap(),
            "{ws}"
        );
    }

    #[test]
    fn sheet_protection_lands_after_sheet_calc_pr_and_before_protected_ranges() {
        let mut pkg = loaded(&format!(
            r#"{ROWS}<sheetCalcPr fullCalcOnLoad="1"/><protectedRanges><protectedRange sqref="A1" name="r"/></protectedRanges><autoFilter ref="A1:B2"/>"#
        ));
        pkg.workbook.sheets[0].set_protected(true);
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(
            at(&ws, "<sheetCalcPr") < at(&ws, "<sheetProtection"),
            "{ws}"
        );
        assert!(
            at(&ws, "<sheetProtection") < at(&ws, "<protectedRanges"),
            "{ws}"
        );
    }

    #[test]
    fn a_new_conditional_format_follows_auto_filter_and_merges() {
        let mut pkg = loaded(&format!(
            r#"{ROWS}<autoFilter ref="A1:B2"/><mergeCells count="1"><mergeCell ref="D1:E1"/></mergeCells>{MARGINS}"#
        ));
        let dxf = crate::sheet::Dxf {
            bold: Some(true),
            ..Default::default()
        };
        pkg.add_conditional_format(0, (0, 0, 1, 0), "greaterThan", "1", None, dxf.clone());
        pkg.add_conditional_format(0, (0, 1, 1, 1), "lessThan", "2", None, dxf);
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(
            at(&ws, "</mergeCells>") < at(&ws, "<conditionalFormatting"),
            "{ws}"
        );
        // The second one is appended after the first.
        assert!(
            at(&ws, r#"sqref="A1:A2""#) < at(&ws, r#"sqref="B1:B2""#),
            "{ws}"
        );
    }

    #[test]
    fn a_new_data_validation_block_precedes_print_options() {
        let mut pkg = loaded(&format!(r#"{ROWS}<printOptions gridLines="1"/>{MARGINS}"#));
        pkg.add_data_validation(0, (0, 0, 1, 0), "whole", "between", "1", Some("9"));
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(
            at(&ws, "<dataValidations") < at(&ws, "<printOptions"),
            "{ws}"
        );
    }

    #[test]
    fn table_parts_skip_an_ext_lst_nested_in_a_cf_rule() {
        let mut pkg = loaded(&format!(
            r#"{ROWS}<conditionalFormatting sqref="A1:A2"><cfRule type="dataBar" priority="1"><dataBar><cfvo type="min"/><cfvo type="max"/><color rgb="FF638EC6"/></dataBar><extLst><ext uri="{{B025F937-C7B1-47D3-B67F-A62EFF666E3E}}"/></extLst></cfRule></conditionalFormatting>{MARGINS}<extLst><ext uri="{{78C0D931-6437-407d-A8EE-F0AAD7539E65}}"/></extLst>"#
        ));
        pkg.add_table(0, (0, 0, 1, 1), true, "TableStyleMedium2")
            .expect("table added");
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        let tp = at(&ws, "<tableParts");
        assert!(at(&ws, "</conditionalFormatting>") < tp, "{ws}");
        assert!(tp < ws.rfind("<extLst").unwrap(), "{ws}");
    }

    fn column_chart() -> crate::sheet::ChartData {
        crate::sheet::ChartData {
            kind: "column".into(),
            categories: vec!["a".into()],
            series: vec![crate::sheet::ChartSeries {
                name: "s".into(),
                values: vec![1.0],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_new_drawing_precedes_controls_wrapped_in_alternate_content() {
        let mc = "http://schemas.openxmlformats.org/markup-compatibility/2006";
        let mut pkg = loaded(&format!(
            r#"{ROWS}{MARGINS}<mc:AlternateContent xmlns:mc="{mc}"><mc:Choice Requires="x14"><controls><control shapeId="1025" r:id="rId9" name="Button 1"/></controls></mc:Choice></mc:AlternateContent>"#
        ));
        pkg.add_chart(0, (0, 3), (10, 8), &column_chart());
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(
            at(&ws, "<drawing ") < at(&ws, "<mc:AlternateContent"),
            "{ws}"
        );
    }

    #[test]
    fn new_sheet_views_follow_sheet_pr_when_there_is_no_dimension() {
        let mut pkg = loaded(&format!(
            r#"<sheetPr><tabColor rgb="FFFF0000"/></sheetPr>{ROWS}"#
        ));
        pkg.workbook.sheets[0].freeze = (1, 0);
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(at(&ws, "</sheetPr>") < at(&ws, "<sheetViews>"), "{ws}");
        assert!(ws.contains(r#"state="frozen""#), "{ws}");
    }

    #[test]
    fn a_prefixed_merge_cells_block_is_replaced_not_duplicated() {
        let mut pkg = new_xlsx();
        pkg.set_part(
            SHEET,
            format!(
                r#"<?xml version="1.0"?><x:worksheet xmlns="{NS}" xmlns:x="{NS}"><x:sheetData><x:row r="1"><x:c r="A1"><x:v>1</x:v></x:c></x:row></x:sheetData><x:mergeCells count="1"><x:mergeCell ref="D1:E1"/></x:mergeCells><x:pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/></x:worksheet>"#
            )
            .into_bytes(),
        );
        let pkg = load_xlsx(&write_zip(&pkg.parts)).expect("load");
        assert_eq!(pkg.workbook.sheets[0].merges.len(), 1);
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert_eq!(ws.matches("mergeCells count=").count(), 1, "{ws}");
        assert_eq!(ws.matches("D1:E1").count(), 1, "{ws}");
    }

    #[test]
    fn a_missing_sheet_data_is_added_before_page_margins() {
        let mut pkg = loaded(MARGINS);
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(7.0));
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(at(&ws, "<sheetData") < at(&ws, "<pageMargins"), "{ws}");
    }

    #[test]
    fn a_new_legacy_drawing_precedes_table_parts() {
        let mut pkg = loaded(ROWS);
        pkg.add_table(0, (0, 0, 1, 1), true, "TableStyleMedium2")
            .expect("table added");
        pkg.set_comment(0, 0, 0, "A", "note");
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(at(&ws, "<legacyDrawing") < at(&ws, "<tableParts"), "{ws}");
    }

    #[test]
    fn the_insert_position_ignores_nested_and_unknown_children() {
        let xml = format!(
            r#"<worksheet xmlns="{NS}"><sheetData/><customSheetViews><customSheetView guid="g"><autoFilter ref="A1"/></customSheetView></customSheetViews><foo/><pageMargins/></worksheet>"#
        );
        // autoFilter ranks before customSheetViews: it goes ahead of them,
        // not next to the nested one.
        assert_eq!(
            worksheet_insert_pos(&xml, "autoFilter"),
            Some(at(&xml, "<customSheetViews"))
        );
        assert_eq!(
            worksheet_insert_pos(&xml, "mergeCells"),
            Some(at(&xml, "<pageMargins"))
        );
        assert_eq!(
            worksheet_insert_pos(&xml, "extLst"),
            Some(at(&xml, "</worksheet>"))
        );
    }

    #[test]
    fn prefixed_row_breaks_follow_a_row_insert_and_stay_bound() {
        let mut pkg = loaded_prefixed(&format!(
            r#"{PREFIXED}<x:rowBreaks count="1" manualBreakCount="1"><x:brk id="3" max="16383" man="1"/></x:rowBreaks>"#
        ));
        crate::edit::insert_rows(&mut pkg.workbook, 0, 0, 1);
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert_names_bound(&ws);
        assert_eq!(count_local(&ws, "rowBreaks"), 1, "{ws}");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let ids: Vec<u32> = re.workbook.sheets[0]
            .row_breaks
            .iter()
            .map(|b| b.id)
            .collect();
        assert_eq!(ids, vec![4]);
    }

    #[test]
    fn the_walk_finds_sheet_data_however_its_close_tag_is_spelled() {
        let tail = r#"<autoFilter ref="A1:B2"/><mergeCells count="1"><mergeCell ref="D1:E1"/></mergeCells>"#;
        for sheet_data in [
            r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData >"#,
            r#"<sheetData><row r="1"><!-- </sheetData> --><c r="A1"><v>1</v></c></row></sheetData>"#,
            r#"<sheetData><row r="1"><c r="A1" t="inlineStr"><is><t><![CDATA[</sheetData>]]></t></is></c></row></sheetData>"#,
            r#"<sheetData><row r="1"><?x </sheetData>?><c r="A1"><v>1</v></c></row></sheetData>"#,
        ] {
            let xml = format!(r#"<worksheet xmlns="{NS}">{sheet_data}{tail}</worksheet>"#);
            let walk = worksheet_children(&xml);
            let names: Vec<&str> = walk.children.iter().map(|c| c.local).collect();
            assert_eq!(names, ["sheetData", "autoFilter", "mergeCells"], "{xml}");
            assert_eq!(walk.close, Some(at(&xml, "</worksheet>")));
            // And a save puts the merges after the filter, with one sheetData.
            let pkg = loaded(&format!("{sheet_data}{tail}"));
            let ws = saved_sheet(&pkg);
            assert_ct_worksheet_order(&ws);
            assert!(at(&ws, "<autoFilter") < at(&ws, "<mergeCells"), "{ws}");
        }
    }

    #[test]
    fn where_a_walk_stops_decides_what_may_be_inserted() {
        // No </worksheet>, but every child read: the list is whole, and new
        // content goes after the last child.
        let truncated = format!(r#"<worksheet xmlns="{NS}"><sheetData/><pageMargins/>"#);
        let walk = worksheet_children(&truncated);
        assert_eq!(walk.close, Some(truncated.len()));
        assert_eq!(
            worksheet_insert_pos(&truncated, "mergeCells"),
            Some(at(&truncated, "<pageMargins"))
        );
        assert_eq!(
            worksheet_insert_pos(&truncated, "drawing"),
            Some(truncated.len())
        );
        // A child cut off with no end tag at all: the walk stops at it. Its
        // start is known, so what ranks before it may still go in; what
        // ranks after it may not.
        let cut_child = format!(r#"<worksheet xmlns="{NS}"><sheetData/><hyperlinks><hyperlink"#);
        let walk = worksheet_children(&cut_child);
        assert!(walk.close.is_none());
        assert_eq!(walk.children.len(), 1);
        assert_eq!(
            walk.stopped,
            Some((at(&cut_child, "<hyperlinks"), "hyperlinks"))
        );
        let before = worksheet_insert_pos(&cut_child, "mergeCells");
        assert_eq!(before, Some(at(&cut_child, "<hyperlinks")));
        assert_eq!(worksheet_insert_pos(&cut_child, "pageMargins"), None);
        assert_eq!(
            put_worksheet_child(&cut_child, "pageMargins", "<pageMargins/>", None, false),
            cut_child
        );
        // A child broken inside but closed by its own end tag is spanned by
        // it, and the walk goes on to the end.
        let resync = format!(
            r#"<worksheet xmlns="{NS}"><sheetData/>{BROKEN_TAIL}<drawing r:id="rId1"/></worksheet>"#
        );
        let walk = worksheet_children(&resync);
        let names: Vec<&str> = walk.children.iter().map(|c| c.local).collect();
        assert_eq!(names, ["sheetData", "headerFooter", "drawing"]);
        assert_eq!(walk.close, Some(at(&resync, "</worksheet>")));
        // A self-closing root has nothing to walk, whatever its prolog holds.
        let empty = format!(r#"<?xml version="1.0"?><!-- </x> --><worksheet xmlns="{NS}"/>"#);
        let walk = worksheet_children(&empty);
        assert!(walk.children.is_empty() && walk.close.is_none());
    }

    /// A child the loader accepts but the parser can't read to its end:
    /// `<oddHeader>` closed as `</OddHeader>`. Its own `</headerFooter>`
    /// still bounds it, so the walk goes on past it.
    const BROKEN_TAIL: &str = "<headerFooter><oddHeader>x</OddHeader></headerFooter>";

    /// The same with no `</headerFooter>`: the walk stops at headerFooter.
    const STOPPED_TAIL: &str = "<headerFooter><oddHeader>x</OddHeader>";

    fn chart() -> crate::sheet::ChartData {
        column_chart()
    }

    fn a3(pkg: &SheetPackage) -> Option<crate::sheet::CellValue> {
        pkg.workbook.sheets[0].cell(2, 0).map(|c| c.value.clone())
    }

    #[test]
    fn cell_edits_survive_a_malformed_child_after_sheet_data() {
        let mut pkg = loaded(&format!(
            r#"{ROWS}<mergeCells count="1"><mergeCell ref="D1:E1"/></mergeCells>{BROKEN_TAIL}"#
        ));
        pkg.workbook.sheets[0].set_cell(2, 0, Cell::number(9.0));
        pkg.workbook.sheets[0].merges.push((5, 0, 5, 1));
        pkg.workbook.sheets[0].set_protected(true);
        let ws = saved_sheet(&pkg);
        assert!(ws.contains(r#"<c r="A3""#), "the edit was dropped: {ws}");
        assert_eq!(count_local(&ws, "sheetData"), 1, "{ws}");
        assert_eq!(count_local(&ws, "mergeCells"), 1, "{ws}");
        assert!(ws.contains("A6:B6"), "{ws}");
        assert_eq!(count_local(&ws, "sheetProtection"), 1, "{ws}");
        assert_ct_worksheet_order(&ws);
        assert!(ws.contains(BROKEN_TAIL), "{ws}");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(a3(&re), Some(crate::sheet::CellValue::Number(9.0)));
        assert!(re.workbook.sheets[0].is_protected());
    }

    #[test]
    fn a_stopped_walk_still_takes_what_ranks_before_the_stop() {
        // The walk stops at headerFooter; merges, protection, CF and DV all
        // rank before it, so their position is known and nothing past the
        // stop can be one of them.
        let mut pkg = loaded(&format!("{ROWS}{MARGINS}{STOPPED_TAIL}"));
        let sheet = &mut pkg.workbook.sheets[0];
        sheet.set_cell(2, 0, Cell::number(9.0));
        sheet.merges.push((5, 0, 5, 1));
        sheet.set_protected(true);
        let dxf = crate::sheet::Dxf {
            bold: Some(true),
            ..Default::default()
        };
        assert!(pkg.add_conditional_format(0, (0, 0, 1, 0), "greaterThan", "1", None, dxf));
        assert!(pkg.add_data_validation(0, (0, 1, 1, 1), "whole", "between", "1", Some("9")));
        let ws = saved_sheet(&pkg);
        for name in [
            "sheetData",
            "sheetProtection",
            "mergeCells",
            "conditionalFormatting",
        ] {
            assert_eq!(count_local(&ws, name), 1, "{name}: {ws}");
        }
        assert!(
            at(&ws, "<dataValidations") < at(&ws, "<headerFooter"),
            "{ws}"
        );
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let s = &re.workbook.sheets[0];
        assert_eq!(a3(&re), Some(crate::sheet::CellValue::Number(9.0)));
        assert_eq!(s.merges.len(), 1);
        assert!(s.is_protected());
        assert_eq!(s.cond_formats.len(), 1);
        assert_eq!(s.validations.len(), 1);
    }

    #[test]
    fn every_add_persists_past_a_child_its_end_tag_still_bounds() {
        let mut pkg = loaded(&format!("{ROWS}{MARGINS}{BROKEN_TAIL}"));
        let dxf = crate::sheet::Dxf {
            bold: Some(true),
            ..Default::default()
        };
        assert!(pkg.add_conditional_format(0, (0, 0, 1, 0), "greaterThan", "1", None, dxf));
        assert!(pkg.add_data_validation(0, (0, 1, 1, 1), "whole", "between", "1", Some("9")));
        assert!(
            pkg.add_table(0, (0, 0, 1, 1), true, "TableStyleMedium2")
                .is_ok()
        );
        assert!(pkg.add_chart(0, (0, 3), (10, 8), &chart()));
        assert!(pkg.set_comment(0, 0, 0, "A", "note"));
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let s = &re.workbook.sheets[0];
        assert_eq!(s.cond_formats.len(), 1);
        assert_eq!(s.validations.len(), 1);
        assert_eq!(re.workbook.tables.len(), 1);
        assert_eq!(s.drawings.len(), 1);
        assert_eq!(re.comments().len(), 1);
    }

    #[test]
    fn can_add_chart_refuses_a_part_damaged_where_drawing_goes() {
        // Asked up front (docxy, when the user inserts a chart it writes at
        // save), it answers as add_chart then does.
        let mut pkg = loaded(&format!("{ROWS}{MARGINS}{STOPPED_TAIL}"));
        let parts = pkg.parts.clone();
        assert!(!pkg.can_add_chart(0));
        assert!(!pkg.add_chart(0, (0, 3), (10, 8), &chart()));
        assert_eq!(pkg.parts, parts);
        assert!(!pkg.can_add_chart(1), "no such sheet");

        let mut pkg = loaded(&format!("{ROWS}{MARGINS}"));
        assert!(pkg.can_add_chart(0));
        assert!(pkg.add_chart(0, (0, 3), (10, 8), &chart()));
        // A second chart joins the drawing part the first made.
        assert!(pkg.can_add_chart(0));
    }

    const DRAWING_RELS: &str = "xl/drawings/_rels/drawing1.xml.rels";
    const SHEET_RELS: &str = "xl/worksheets/_rels/sheet1.xml.rels";

    /// A sheet that already holds one chart, so the next joins its drawing.
    fn with_a_chart() -> SheetPackage {
        let mut pkg = loaded(&format!("{ROWS}{MARGINS}"));
        assert!(pkg.add_chart(0, (0, 3), (10, 8), &chart()));
        let pkg = load_xlsx(&save_xlsx(&pkg)).expect("reload");
        assert!(pkg.chart_host(0).is_some());
        pkg
    }

    /// `name` cut short just before its `</Relationships>`.
    fn truncate_rels(pkg: &mut SheetPackage, name: &str) {
        let xml = String::from_utf8(pkg.part(name).unwrap().to_vec()).unwrap();
        let cut = xml.rfind("</Relationships>").expect("a rels part");
        pkg.set_part(name, xml.as_bytes()[..cut].to_vec());
    }

    #[test]
    fn can_add_chart_refuses_a_broken_drawing_rels_part() {
        let mut pkg = with_a_chart();
        truncate_rels(&mut pkg, DRAWING_RELS);
        assert!(!pkg.can_add_chart(0));
    }

    #[test]
    fn can_add_chart_refuses_a_broken_worksheet_rels_part() {
        // No drawing yet, so the worksheet needs a new rel to one.
        let mut pkg = loaded(&format!("{ROWS}{MARGINS}"));
        pkg.set_part(
            SHEET_RELS,
            format!(r#"<Relationships xmlns="{R}"/>"#).into_bytes(),
        );
        assert!(!pkg.can_add_chart(0));
    }

    #[test]
    fn a_broken_worksheet_rels_part_that_already_names_the_drawing_takes_a_chart() {
        // The rel to the host drawing is there to reuse; nothing new goes in.
        let mut pkg = with_a_chart();
        truncate_rels(&mut pkg, SHEET_RELS);
        assert!(pkg.can_add_chart(0));
        assert!(pkg.add_chart(0, (12, 3), (20, 8), &chart()));
        let drawing =
            String::from_utf8(pkg.part("xl/drawings/drawing1.xml").unwrap().to_vec()).unwrap();
        assert_eq!(drawing.matches("<xdr:graphicFrame").count(), 2, "{drawing}");
    }

    #[test]
    fn add_chart_leaves_the_package_alone_when_a_rels_part_is_broken() {
        let mut pkg = with_a_chart();
        truncate_rels(&mut pkg, DRAWING_RELS);
        let parts = pkg.parts.clone();
        assert!(!pkg.add_chart(0, (12, 3), (20, 8), &chart()));
        assert_eq!(pkg.parts, parts);

        let mut pkg = loaded(&format!("{ROWS}{MARGINS}"));
        pkg.set_part(SHEET_RELS, b"<Relationships xmlns=\"x\">".to_vec());
        let parts = pkg.parts.clone();
        assert!(!pkg.add_chart(0, (0, 3), (10, 8), &chart()));
        assert_eq!(pkg.parts, parts);
    }

    #[test]
    fn a_new_drawings_orphaned_rels_part_that_cannot_take_the_chart_refuses_it() {
        // drawing1.xml is free, but a rels part for it is left behind,
        // self-closed: the chart's rel could not go in.
        let mut pkg = loaded(&format!("{ROWS}{MARGINS}"));
        pkg.set_part(
            DRAWING_RELS,
            format!(r#"<Relationships xmlns="{R}"/>"#).into_bytes(),
        );
        assert!(pkg.part("xl/drawings/drawing1.xml").is_none());
        let parts = pkg.parts.clone();
        assert!(!pkg.can_add_chart(0));
        assert!(!pkg.add_chart(0, (0, 3), (10, 8), &chart()));
        assert_eq!(pkg.parts, parts);
    }

    #[test]
    fn a_sheet_with_no_part_entry_takes_no_chart() {
        // The model has the sheet, the package no part name for it.
        let mut pkg = loaded(&format!("{ROWS}{MARGINS}"));
        pkg.sheet_parts.pop();
        let parts = pkg.parts.clone();
        assert!(!pkg.can_add_chart(0));
        assert!(!pkg.add_chart(0, (0, 3), (10, 8), &chart()));
        assert_eq!(pkg.parts, parts);
    }

    #[test]
    fn a_sheet_with_no_place_for_data_validations_says_so() {
        // The walk stops before dataValidations' rank: a rule added to the
        // model would vanish on save, so hosts ask first (#689 review).
        let early = loaded(&format!(
            r#"{ROWS}<autoFilter ref="A1:B2"><filterColumn colId="0">{MARGINS}"#
        ));
        assert!(!early.takes_validations(0));
        assert!(!early.takes_validations(9), "no such sheet");
        assert!(loaded(&format!("{ROWS}{MARGINS}")).takes_validations(0));
    }

    #[test]
    fn clearing_the_anchor_cell_keeps_relative_formulas_right_through_a_save() {
        let mut pkg = loaded(&format!(
            r#"{ROWS}<dataValidations count="1"><dataValidation type="custom" showErrorMessage="1" sqref="B2:B10"><formula1>B2&gt;A2</formula1></dataValidation></dataValidations>{MARGINS}"#
        ));
        crate::validation::clear_validation(&mut pkg.workbook.sheets[0], (1, 1, 1, 1));
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let dv = &re.workbook.sheets[0].validations[0];
        assert_eq!(dv.ranges, vec![(2, 1, 9, 1)]);
        assert_eq!(dv.formula1, "B3>A3");
        // B3 is checked as B3>A3, as it was before the clear.
        let part = String::from_utf8(re.part(SHEET).unwrap().to_vec()).unwrap();
        assert!(part.contains(r#"sqref="B3:B10""#), "{part}");
    }

    #[test]
    fn an_add_with_no_known_position_is_refused_and_changes_nothing() {
        // The walk stops at headerFooter; drawing, legacyDrawing and
        // tableParts rank after it, where the part can't be read.
        let mut pkg = loaded(&format!("{ROWS}{MARGINS}{STOPPED_TAIL}"));
        let parts = pkg.parts.clone();
        let workbook = pkg.workbook.clone();
        assert!(
            pkg.add_table(0, (0, 0, 1, 1), true, "TableStyleMedium2")
                .is_err()
        );
        assert!(!pkg.add_chart(0, (0, 3), (10, 8), &chart()));
        assert!(!pkg.set_comment(0, 0, 0, "A", "note"));
        // A walk stopped before conditionalFormatting's rank has no place for
        // a rule either, and can't vouch that a clear saw every block.
        // It holds a rule, so a clear that touched only one side would show.
        let mut early = loaded(&format!(
            r#"{ROWS}<conditionalFormatting sqref="A1"><cfRule type="expression" priority="1"><formula>TRUE</formula></cfRule></conditionalFormatting><autoFilter ref="A1:B2"><filterColumn colId="0">{MARGINS}"#
        ));
        assert_eq!(early.workbook.sheets[0].cond_formats.len(), 1);
        let early_parts = early.parts.clone();
        let dxf = crate::sheet::Dxf {
            bold: Some(true),
            ..Default::default()
        };
        assert!(!early.add_conditional_format(0, (0, 0, 1, 0), "greaterThan", "1", None, dxf));
        assert!(!early.clear_conditional_formats(0));
        // Nothing was written anywhere, and the model says the same.
        assert_eq!(pkg.parts, parts);
        assert_eq!(pkg.workbook.tables.len(), workbook.tables.len());
        assert_eq!(pkg.workbook.sheets[0].drawings.len(), 0);
        assert!(pkg.comments().is_empty());
        assert_eq!(early.parts, early_parts);
        assert_eq!(early.workbook.sheets[0].cond_formats.len(), 1);
        // A threaded comment needs the same <legacyDrawing> as a note.
        assert!(!pkg.add_threaded_comment(0, 0, 0, "Ana", "Hi", "2024-01-02T03:04:05Z"));
        assert_eq!(pkg.parts, parts);
        assert!(pkg.comments().is_empty());
    }

    #[test]
    fn a_chart_whose_host_drawing_cannot_take_an_anchor_writes_nothing() {
        let mut pkg = loaded(ROWS);
        assert!(pkg.add_chart(0, (0, 3), (10, 8), &chart()));
        // Truncate the host drawing part: no </xdr:wsDr>, no self-closed root.
        let host = pkg.workbook.sheets[0].drawing_part.clone().expect("host");
        let xml = String::from_utf8_lossy(pkg.part(&host).unwrap()).into_owned();
        let cut = &xml[..xml.rfind("</xdr:wsDr>").unwrap()];
        pkg.set_part(&host, cut.as_bytes().to_vec());
        let parts = pkg.parts.clone();
        let drawings = pkg.workbook.sheets[0].drawings.len();
        assert!(!pkg.add_chart(0, (12, 3), (20, 8), &chart()));
        assert_eq!(pkg.parts, parts);
        assert_eq!(pkg.workbook.sheets[0].drawings.len(), drawings);
    }

    #[test]
    fn removing_the_last_note_strips_a_legacy_drawing_behind_a_stopped_walk() {
        let mut pkg = loaded(&format!("{ROWS}{MARGINS}"));
        assert!(pkg.set_comment(0, 0, 0, "A", "note"));
        // Break the part so the walk stops at headerFooter, before the
        // <legacyDrawing> that ranks after it.
        let ws = String::from_utf8_lossy(pkg.part(SHEET).unwrap()).into_owned();
        let at_ld = at(&ws, "<legacyDrawing");
        let broken = format!("{}{STOPPED_TAIL}{}", &ws[..at_ld], &ws[at_ld..]);
        pkg.set_part(SHEET, broken.into_bytes());
        pkg.remove_comment(0, 0, 0);
        let ws = String::from_utf8_lossy(pkg.part(SHEET).unwrap()).into_owned();
        assert_eq!(count_local(&ws, "legacyDrawing"), 0, "{ws}");
        // A sheet with no legacyDrawing is left alone.
        let plain = format!(r#"<worksheet xmlns="{NS}"><sheetData/>{MARGINS}</worksheet>"#);
        assert_eq!(remove_worksheet_singleton(&plain, "legacyDrawing"), plain);
    }

    #[test]
    fn a_resync_is_refused_when_the_end_tag_could_be_someone_elses() {
        // A comment holding a literal </headerFooter>.
        let commented = format!(
            r#"<worksheet xmlns="{NS}"><sheetData/><headerFooter><oddHeader>x</OddHeader><!-- </headerFooter> --></headerFooter><drawing r:id="rId1"/></worksheet>"#
        );
        let walk = worksheet_children(&commented);
        assert_eq!(walk.stopped.map(|(_, n)| n), Some("headerFooter"));
        assert!(walk.close.is_none());
        // An unclosed top-level <autoFilter> ahead of a custom view with its
        // own: the nested </autoFilter> must not end the broken one, or the
        // walk would resume inside the custom view.
        let nested = format!(
            r#"<worksheet xmlns="{NS}"><sheetData/><autoFilter ref="A1:B2"><filterColumn colId="0"><customSheetViews><customSheetView guid="g"><autoFilter ref="A1"></autoFilter><extLst/></customSheetView></customSheetViews>{MARGINS}</worksheet>"#
        );
        let walk = worksheet_children(&nested);
        assert_eq!(walk.stopped.map(|(_, n)| n), Some("autoFilter"));
        assert_eq!(walk.children.len(), 1);
        for tag in [
            "conditionalFormatting",
            "mergeCells",
            "dataValidations",
            "tableParts",
        ] {
            assert_eq!(worksheet_insert_pos(&nested, tag), None, "{tag}");
        }
    }

    #[test]
    fn cell_edits_survive_a_missing_worksheet_end_tag() {
        let mut pkg = new_xlsx();
        pkg.set_part(
            SHEET,
            format!(r#"<?xml version="1.0"?><worksheet xmlns="{NS}">{ROWS}{MARGINS}"#).into_bytes(),
        );
        let mut pkg = load_xlsx(&write_zip(&pkg.parts)).expect("load");
        pkg.workbook.sheets[0].set_cell(2, 0, Cell::number(9.0));
        let ws = saved_sheet(&pkg);
        assert!(ws.contains(r#"<c r="A3""#), "the edit was dropped: {ws}");
        assert_eq!(count_local(&ws, "sheetData"), 1, "{ws}");
    }

    #[test]
    fn cell_edits_survive_a_malformed_child_before_sheet_data() {
        // <sheetViews> never closes: the walk stops before sheetData, which is
        // then found by its tags.
        let mut pkg = loaded(&format!(
            r#"<sheetViews><sheetView workbookViewId="0">{ROWS}{MARGINS}"#
        ));
        assert_eq!(
            worksheet_children(&String::from_utf8_lossy(pkg.part(SHEET).unwrap()))
                .stopped
                .map(|(_, n)| n),
            Some("sheetViews")
        );
        pkg.workbook.sheets[0].set_cell(2, 0, Cell::number(9.0));
        let ws = saved_sheet(&pkg);
        assert!(ws.contains(r#"<c r="A3""#), "the edit was dropped: {ws}");
        assert_eq!(count_local(&ws, "sheetData"), 1, "{ws}");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(a3(&re), Some(crate::sheet::CellValue::Number(9.0)));
    }

    #[test]
    fn clearing_conditional_formats_past_a_stop_after_them_clears() {
        // The walk stops at headerFooter, which ranks after every place a
        // conditionalFormatting may stand: all of them were seen.
        let mut pkg = loaded(&format!(
            r#"{ROWS}<conditionalFormatting sqref="A1"><cfRule type="expression" priority="1"><formula>TRUE</formula></cfRule></conditionalFormatting>{STOPPED_TAIL}"#
        ));
        assert_eq!(pkg.workbook.sheets[0].cond_formats.len(), 1);
        assert!(pkg.clear_conditional_formats(0));
        let ws = saved_sheet(&pkg);
        assert_eq!(count_local(&ws, "conditionalFormatting"), 0, "{ws}");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert!(re.workbook.sheets[0].cond_formats.is_empty());
    }

    #[test]
    fn a_self_closing_worksheet_is_opened_not_appended_to() {
        let mut pkg = new_xlsx();
        pkg.set_part(
            SHEET,
            format!(r#"<?xml version="1.0"?><worksheet xmlns="{NS}"/>"#).into_bytes(),
        );
        let mut pkg = load_xlsx(&write_zip(&pkg.parts)).expect("load");
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(5.0));
        let ws = saved_sheet(&pkg);
        assert!(ws.trim_end().ends_with("</worksheet>"), "{ws}");
        assert_eq!(count_local(&ws, "sheetData"), 1, "{ws}");
        assert_names_bound(&ws);
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(
            re.workbook.sheets[0].cell(0, 0).map(|c| c.value.clone()),
            Some(crate::sheet::CellValue::Number(5.0))
        );
    }

    #[test]
    fn clearing_conditional_formats_removes_prefixed_blocks() {
        let mut pkg = loaded_prefixed(
            r#"<x:sheetData/><x:conditionalFormatting sqref="A1"><x:cfRule type="expression" priority="1"><x:formula>TRUE</x:formula></x:cfRule></x:conditionalFormatting><x:conditionalFormatting sqref="B1"><x:cfRule type="expression" priority="2"><x:formula>TRUE</x:formula></x:cfRule></x:conditionalFormatting><x:pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>"#,
        );
        assert_eq!(pkg.workbook.sheets[0].cond_formats.len(), 2);
        pkg.clear_conditional_formats(0);
        let ws = saved_sheet(&pkg);
        assert_eq!(count_local(&ws, "conditionalFormatting"), 0, "{ws}");
        assert_eq!(count_local(&ws, "pageMargins"), 1, "{ws}");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert!(re.workbook.sheets[0].cond_formats.is_empty());
    }

    /// A ratchet, not the #597 test: no corpus sheet that was in schema order
    /// comes out of a load and save out of order.
    #[test]
    fn corpus_sheets_in_schema_order_stay_in_order() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../corpus/xlsx");
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).expect("corpus/xlsx exists") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("xlsx") {
                continue;
            }
            let pkg = load_xlsx(&std::fs::read(&path).unwrap()).expect("corpus loads");
            let re = load_xlsx(&save_xlsx(&pkg)).expect("corpus reloads");
            for name in &pkg.sheet_parts {
                let before = String::from_utf8_lossy(pkg.part(name).unwrap()).into_owned();
                if in_ct_worksheet_order(&before).is_err() {
                    continue;
                }
                let after = String::from_utf8_lossy(re.part(name).unwrap()).into_owned();
                if let Err(e) = in_ct_worksheet_order(&after) {
                    panic!("{}: {name}: {e}", path.display());
                }
                checked += 1;
            }
        }
        assert!(checked > 0, "no corpus sheets checked");
    }
    /// An Excel-shaped outline: rows 2..=4 and 6..=7 nested in 2..=8 (row 4's
    /// group collapsed), columns B..C grouped, summaries above.
    const OUTLINED: &str = r#"<sheetPr><outlinePr summaryBelow="0" summaryRight="1"/></sheetPr><dimension ref="A1:D9"/><sheetFormatPr defaultRowHeight="15" outlineLevelRow="2" outlineLevelCol="1"/><cols><col min="2" max="3" width="9.140625" outlineLevel="1" collapsed="1"/></cols><sheetData><row r="1"><c r="A1"><v>1</v></c></row><row r="2" outlineLevel="1" collapsed="1"><c r="A2"><v>2</v></c></row><row r="3" hidden="1" outlineLevel="2"><c r="A3"><v>3</v></c></row><row r="4" hidden="1" outlineLevel="2"><c r="A4"><v>4</v></c></row><row r="5" outlineLevel="1"><c r="A5"><v>5</v></c></row><row r="6" outlineLevel="2"><c r="A6"><v>6</v></c></row><row r="7" outlineLevel="2"><c r="A7"><v>7</v></c></row><row r="8" outlineLevel="1"><c r="A8"><v>8</v></c></row></sheetData>"#;

    #[test]
    fn an_excel_outline_loads_and_round_trips() {
        use crate::outline::{Axis, groups};
        let pkg = loaded(&format!("{OUTLINED}{MARGINS}"));
        let s = &pkg.workbook.sheets[0];
        assert!(!s.outline.summary_below && s.outline.summary_right);
        assert_eq!(
            (0..9).map(|r| s.row_outline(r)).collect::<Vec<_>>(),
            [0, 1, 2, 2, 1, 2, 2, 1, 0]
        );
        assert!(s.row_collapsed(1) && s.row_hidden(2));
        assert_eq!(
            (s.col_outline(1), s.col_outline(2), s.col_outline(3)),
            (1, 1, 0)
        );
        assert!(s.col_collapsed(1));
        // Summaries above: row 2 heads the inner group 3..=4.
        let g = groups(s, Axis::Rows);
        let inner = g.iter().find(|g| g.start == 2).unwrap();
        assert_eq!((inner.summary, inner.collapsed), (Some(1), true));
        // Untouched, the outline elements stay as they were.
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(
            ws.contains(r#"<outlinePr summaryBelow="0" summaryRight="1"/>"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"outlineLevelRow="2" outlineLevelCol="1""#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"outlineLevel="1" collapsed="1"/></cols>"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<row r="2" outlineLevel="1" collapsed="1">"#),
            "{ws}"
        );
    }

    #[test]
    fn outline_settings_and_levels_are_written_back() {
        let mut pkg = loaded(&format!("{OUTLINED}{MARGINS}"));
        let s = &mut pkg.workbook.sheets[0];
        s.outline.summary_below = true;
        s.outline.summary_right = false;
        crate::outline::group(s, crate::outline::Axis::Rows, 2, 3).unwrap(); // level 3
        crate::outline::ungroup(s, crate::outline::Axis::Cols, 1, 2).unwrap();
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        // Patched in place: attribute order is the patcher's.
        let pr = &ws[at(&ws, "<outlinePr")..];
        let pr = &pr[..pr.find("/>").unwrap()];
        assert!(
            pr.contains(r#"summaryBelow="1""#) && pr.contains(r#"summaryRight="0""#),
            "{ws}"
        );
        assert!(ws.contains(r#"outlineLevelRow="3""#), "{ws}");
        assert!(!ws.contains("outlineLevelCol"), "level 0 is left out: {ws}");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let s = &re.workbook.sheets[0];
        assert!(s.outline.summary_below && !s.outline.summary_right);
        assert_eq!(s.row_outline(2), 3);
        assert_eq!(s.col_outline(1), 0);
        assert!(!s.col_collapsed(1), "the flag went with the group");
    }

    #[test]
    fn a_new_outline_adds_sheet_format_pr_and_outline_pr_in_place() {
        // No sheetPr, no sheetFormatPr.
        let mut pkg = loaded(&format!(r#"<dimension ref="A1:B2"/>{ROWS}{MARGINS}"#));
        let s = &mut pkg.workbook.sheets[0];
        crate::outline::group(s, crate::outline::Axis::Rows, 0, 1).unwrap();
        crate::outline::group(s, crate::outline::Axis::Cols, 0, 0).unwrap();
        s.outline.summary_below = false;
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(
            ws.contains(
                r#"<sheetFormatPr defaultRowHeight="15" outlineLevelRow="1" outlineLevelCol="1"/>"#
            ),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<sheetPr><outlinePr summaryBelow="0" summaryRight="1"/></sheetPr>"#),
            "{ws}"
        );
        // An existing sheetPr keeps tabColor first and pageSetUpPr last.
        let mut pkg = loaded(&format!(
            r#"<sheetPr><tabColor rgb="FFFF0000"/><pageSetUpPr fitToPage="1"/></sheetPr>{ROWS}{MARGINS}"#
        ));
        pkg.workbook.sheets[0].outline.summary_right = false;
        let ws = saved_sheet(&pkg);
        assert!(
            ws.contains(r#"<tabColor rgb="FFFF0000"/><outlinePr summaryBelow="1" summaryRight="0"/><pageSetUpPr"#),
            "{ws}"
        );
    }

    #[test]
    fn clearing_the_outline_drops_the_level_attributes() {
        let mut pkg = loaded(&format!("{OUTLINED}{MARGINS}"));
        crate::outline::clear_outline(&mut pkg.workbook.sheets[0]).unwrap();
        let ws = saved_sheet(&pkg);
        assert!(
            ws.contains(r#"<sheetFormatPr defaultRowHeight="15"/>"#),
            "{ws}"
        );
        assert!(!ws.contains("outlineLevel"), "{ws}");
        assert!(!ws.contains("collapsed"), "{ws}");
    }

    #[test]
    fn an_untouched_outline_pr_is_kept_byte_for_byte() {
        // oracle-basic.xlsx spells it with a space before `/>` and an empty
        // pageSetUpPr; nothing about the outline changes, so neither do they.
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../corpus/xlsx/oracle-basic.xlsx"
        ))
        .unwrap();
        let pkg = load_xlsx(&data).unwrap();
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let ws = String::from_utf8(re.part(SHEET).unwrap().to_vec()).unwrap();
        assert!(
            ws.contains(r#"<sheetPr><outlinePr summaryBelow="1" summaryRight="1" /><pageSetUpPr /></sheetPr>"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<sheetFormatPr baseColWidth="8" defaultRowHeight="15" />"#),
            "{ws}"
        );
    }

    #[test]
    fn rows_and_columns_built_in_code_are_well_formed() {
        // A test or a UI may write attributes without the loader's leading
        // space; the writer must still separate them from `r="…"`.
        let mut pkg = new_xlsx();
        let s = &mut pkg.workbook.sheets[0];
        s.set_cell(0, 0, Cell::number(1.0));
        s.row_attrs.insert(0, "hidden=\"1\"".into());
        s.row_attrs.insert(3, "outlineLevel=\"1\"".into());
        s.col_defs.push(crate::sheet::ColDef {
            min: 2,
            max: 2,
            width: None,
            attrs: "hidden=\"1\"".into(),
            default_width: false,
        });
        let ws = saved_sheet(&pkg);
        assert!(ws.contains(r#"<row r="1" hidden="1">"#), "{ws}");
        assert!(ws.contains(r#"<row r="4" outlineLevel="1"/>"#), "{ws}");
        assert!(ws.contains(r#"<col min="3" max="3" hidden="1"/>"#), "{ws}");
    }

    #[test]
    fn a_loaded_col_without_a_width_keeps_its_spelling() {
        let pkg = loaded(&format!(
            r#"<cols><col min="2" max="2" style="5"/></cols>{ROWS}{MARGINS}"#
        ));
        assert!(saved_sheet(&pkg).contains(r#"<cols><col min="2" max="2" style="5"/></cols>"#));
        // Grouping elsewhere doesn't touch it either.
        let mut pkg = pkg;
        crate::outline::group(
            &mut pkg.workbook.sheets[0],
            crate::outline::Axis::Cols,
            4,
            4,
        )
        .unwrap();
        let ws = saved_sheet(&pkg);
        assert!(
            ws.contains(r#"<col min="2" max="2" style="5"/><col min="5" max="5" width="9.140625" outlineLevel="1"/>"#),
            "{ws}"
        );
    }

    #[test]
    fn grouped_columns_keep_a_width_and_ungrouped_ones_go() {
        use crate::outline::{Axis, group, ungroup};
        // No `<cols>` at all: the definitions Group makes need a width, or
        // Excel opens the columns zero wide.
        let mut pkg = loaded(&format!(r#"<dimension ref="A1:B2"/>{ROWS}{MARGINS}"#));
        group(&mut pkg.workbook.sheets[0], Axis::Cols, 2, 3).unwrap();
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(
            ws.contains(r#"<cols><col min="3" max="4" width="9.140625" outlineLevel="1"/></cols>"#),
            "{ws}"
        );
        // Ungrouped in the same session, nothing is left to say: the element
        // goes.
        let mut fresh = pkg.clone();
        ungroup(&mut fresh.workbook.sheets[0], Axis::Cols, 2, 3).unwrap();
        assert!(fresh.workbook.sheets[0].col_defs.is_empty());
        let ws = saved_sheet(&fresh);
        assert!(!ws.contains("<col"), "{ws}");
        // After a reload it is still a default-width column: as wide as its
        // neighbours, saved again without customWidth, and gone once
        // ungrouped.
        let mut pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let s = &pkg.workbook.sheets[0];
        assert_eq!(s.col_width(2), s.col_width(5));
        let ws = saved_sheet(&pkg);
        assert!(
            ws.contains(r#"<cols><col min="3" max="4" width="9.140625" outlineLevel="1"/></cols>"#),
            "{ws}"
        );
        ungroup(&mut pkg.workbook.sheets[0], Axis::Cols, 2, 3).unwrap();
        let ws = saved_sheet(&pkg);
        assert!(!ws.contains("<col"), "{ws}");
        // A width the user set stays custom.
        let mut pkg = loaded(&format!(
            r#"<cols><col min="2" max="2" width="9.140625" customWidth="1"/></cols>{ROWS}{MARGINS}"#
        ));
        assert_eq!(pkg.workbook.sheets[0].col_defs[0].width, Some(9.140625));
        pkg.workbook.sheets[0].set_col_width(3, 20.0);
        assert!(
            saved_sheet(&pkg)
                .contains(r#"<col min="2" max="2" width="9.140625" customWidth="1"/>"#)
        );
        assert!(!ws.contains("outlineLevel"), "{ws}");
        // A sheet's own default width is the one written.
        let mut pkg = loaded(&format!(
            r#"<sheetFormatPr defaultColWidth="12.5" defaultRowHeight="15"/>{ROWS}{MARGINS}"#
        ));
        group(&mut pkg.workbook.sheets[0], Axis::Cols, 0, 0).unwrap();
        let ws = saved_sheet(&pkg);
        assert!(
            ws.contains(r#"<col min="1" max="1" width="12.5" outlineLevel="1"/>"#),
            "{ws}"
        );
    }

    const CONSOLIDATED: &str = r#"<dataConsolidate function="average" startLabels="1" topLabels="1"><dataRefs count="3"><dataRef ref="A1:B2" sheet="My sheet"/><dataRef name="Totals"/><dataRef ref="A1" sheet="X" r:id="rId9"/></dataRefs></dataConsolidate>"#;

    #[test]
    fn data_consolidate_settings_load_and_an_untouched_element_keeps_its_bytes() {
        let pkg = loaded(&format!(
            r#"{ROWS}<autoFilter ref="A1:B2"/>{CONSOLIDATED}<mergeCells count="1"><mergeCell ref="D1:E1"/></mergeCells>{MARGINS}"#
        ));
        let s = &pkg.workbook.sheets[0];
        let want = crate::edit::ConsolidateSettings {
            func: crate::edit::SubtotalFunc::Average,
            // Another workbook's reference (r:id) is left out.
            refs: vec!["'My sheet'!$A$1:$B$2".into(), "Totals".into()],
            top_row: true,
            left_col: true,
            links: false,
        };
        assert_eq!(s.consolidate.as_ref(), Some(&want));
        assert_eq!(s.consolidate_loaded, s.consolidate);
        let ws = saved_sheet(&pkg);
        assert!(ws.contains(CONSOLIDATED), "{ws}");
        // Nothing else on the sheet is a Consolidate setting.
        let plain = loaded(&format!("{ROWS}{MARGINS}"));
        assert_eq!(plain.workbook.sheets[0].consolidate, None);
    }

    #[test]
    fn settings_are_remembered_and_round_trip_through_data_consolidate() {
        let mut pkg = loaded(&format!(
            r#"{ROWS}<autoFilter ref="A1:B2"/>{CONSOLIDATED}<mergeCells count="1"><mergeCell ref="D1:E1"/></mergeCells>{MARGINS}"#
        ));
        let changed = crate::edit::ConsolidateSettings {
            func: crate::edit::SubtotalFunc::CountNums,
            refs: vec![
                "'My sheet'!$A$1:$B$2".into(),
                "East!$C$3".into(),
                "Totals".into(),
            ],
            top_row: false,
            left_col: true,
            links: true,
        };
        pkg.workbook.sheets[0].consolidate = Some(changed.clone());
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(
            ws.contains(r#"<dataConsolidate function="countNums" leftLabels="1" link="1"><dataRefs count="3"><dataRef ref="A1:B2" sheet="My sheet"/><dataRef ref="C3" sheet="East"/><dataRef name="Totals"/></dataRefs></dataConsolidate>"#),
            "{ws}"
        );
        assert_eq!(ws.matches("<dataConsolidate").count(), 1, "{ws}");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(re.workbook.sheets[0].consolidate, Some(changed));
        // Cleared, the element goes.
        pkg.workbook.sheets[0].consolidate = None;
        let ws = saved_sheet(&pkg);
        assert!(!ws.contains("dataConsolidate"), "{ws}");
    }

    #[test]
    fn a_new_data_consolidate_lands_in_schema_order() {
        let mut pkg = loaded(&format!(
            r#"{ROWS}<sortState ref="A2:B2"><sortCondition ref="A2:A2"/></sortState><mergeCells count="1"><mergeCell ref="D1:E1"/></mergeCells>{MARGINS}"#
        ));
        pkg.workbook.sheets[0].consolidate = Some(crate::edit::ConsolidateSettings {
            refs: vec!["Sheet1!$A$1:$B$2".into()],
            ..Default::default()
        });
        let ws = saved_sheet(&pkg);
        assert_ct_worksheet_order(&ws);
        assert!(
            ws.contains(r#"<dataConsolidate><dataRefs count="1"><dataRef ref="A1:B2" sheet="Sheet1"/></dataRefs></dataConsolidate>"#),
            "{ws}"
        );
        assert!(
            at(&ws, "</sortState>") < at(&ws, "<dataConsolidate"),
            "{ws}"
        );
        assert!(
            at(&ws, "</dataConsolidate>") < at(&ws, "<mergeCells"),
            "{ws}"
        );
    }

    /// DAT-CASE-035: a linked consolidation as Excel leaves it (formulas
    /// with cached values, hidden level-1 detail rows, collapsed summary
    /// rows), saved and reopened, recalculates to the values it cached.
    #[test]
    fn a_saved_linked_consolidation_recalculates_to_its_cached_values() {
        let mut pkg = new_xlsx_sheets(&["East".into(), "West".into(), "Summary".into()]);
        let part = |pkg: &SheetPackage, i: usize| pkg.sheet_parts[i].clone();
        let sheet = |rows: &str| {
            format!(
                r#"<?xml version="1.0"?><worksheet xmlns="{NS}" xmlns:r="{R}"><sheetData>{rows}</sheetData>{MARGINS}</worksheet>"#
            )
        };
        let east = sheet(
            r#"<row r="1"><c r="B1" t="inlineStr"><is><t>Jan</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>A</t></is></c><c r="B2"><v>1</v></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>B</t></is></c><c r="B3"><v>3</v></c></row>"#,
        );
        let west = sheet(
            r#"<row r="1"><c r="B1" t="inlineStr"><is><t>jan</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>a</t></is></c><c r="B2"><v>40</v></c></row>"#,
        );
        let summary = format!(
            r#"<?xml version="1.0"?><worksheet xmlns="{NS}" xmlns:r="{R}"><sheetPr><outlinePr summaryBelow="1"/></sheetPr><sheetFormatPr defaultRowHeight="15" outlineLevelRow="1"/><sheetData><row r="1"><c r="C1" t="inlineStr"><is><t>Jan</t></is></c></row><row r="2" hidden="1" outlineLevel="1"><c r="B2" t="inlineStr"><is><t>Book1</t></is></c><c r="C2"><f>East!$B$2</f><v>1</v></c></row><row r="3" hidden="1" outlineLevel="1"><c r="B3" t="inlineStr"><is><t>Book1</t></is></c><c r="C3"><f>West!$B$2</f><v>40</v></c></row><row r="4" collapsed="1"><c r="A4" t="inlineStr"><is><t>A</t></is></c><c r="C4"><f>SUM(C2:C3)</f><v>41</v></c></row><row r="5" hidden="1" outlineLevel="1"><c r="B5" t="inlineStr"><is><t>Book1</t></is></c><c r="C5"><f>East!$B$3</f><v>3</v></c></row><row r="6" collapsed="1"><c r="A6" t="inlineStr"><is><t>B</t></is></c><c r="C6"><f>SUM(C5)</f><v>3</v></c></row></sheetData><dataConsolidate topLabels="1" leftLabels="1" link="1"><dataRefs count="2"><dataRef ref="A1:B3" sheet="East"/><dataRef ref="A1:B2" sheet="West"/></dataRefs></dataConsolidate>{MARGINS}</worksheet>"#
        );
        for (i, xml) in [east, west, summary].into_iter().enumerate() {
            let p = part(&pkg, i);
            pkg.set_part(&p, xml.into_bytes());
        }
        let mut pkg = load_xlsx(&write_zip(&pkg.parts)).expect("load");
        let mut re = load_xlsx(&save_xlsx(&pkg)).expect("reload");
        for p in [&mut pkg, &mut re] {
            let cached: Vec<CellValue> = ["C2", "C3", "C4", "C5", "C6"]
                .iter()
                .map(|n| {
                    let (r, c) = crate::sheet::parse_cell_name(n).unwrap();
                    p.workbook.sheets[2].cell(r, c).unwrap().value.clone()
                })
                .collect();
            let mut eng = crate::engine::Engine::new(&p.workbook);
            eng.recalc_all(&mut p.workbook);
            let s = &p.workbook.sheets[2];
            let now: Vec<CellValue> = [(1, 2), (2, 2), (3, 2), (4, 2), (5, 2)]
                .iter()
                .map(|&(r, c)| s.cell(r, c).unwrap().value.clone())
                .collect();
            assert_eq!(now, cached);
            assert_eq!(now[2], CellValue::Number(41.0));
            assert!(s.row_hidden(1) && s.row_outline(1) == 1 && s.row_collapsed(3));
            assert!(s.consolidate.as_ref().is_some_and(|c| c.links));
        }
    }
}

/// #602: a Strict Open XML workbook stays strict. What the writer creates uses
/// the Strict namespaces; what it doesn't touch keeps its bytes.
#[cfg(test)]
mod strict_tests {
    use super::*;
    use crate::sheet::{Cell, CellValue};

    const T_SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    const T_RELS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const TRANSITIONAL_URIS: &[&str] = &[
        T_SML,
        T_RELS,
        "http://schemas.openxmlformats.org/drawingml/2006/main",
        "http://schemas.openxmlformats.org/drawingml/2006/chart",
        "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing",
    ];
    const APP_REL: &str = r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/>"#;
    const APP_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>Microsoft Excel</Application></Properties>"#;

    /// A Strict workbook as openpyxl-then-retagged writes it: strict parts and
    /// relationship types, inline strings, no shared-strings part. Like
    /// Excel's own Strict files, its docProps relationship stays transitional.
    fn strict_xlsx() -> Vec<u8> {
        let base = new_xlsx();
        let strict = |s: &str| s.replace(T_SML, STRICT.sml).replace(T_RELS, STRICT.rels);
        let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
        for (name, bytes) in &base.parts {
            let text = String::from_utf8(bytes.clone()).unwrap();
            let text = match name.as_str() {
                "xl/sharedStrings.xml" => continue,
                "[Content_Types].xml" => text.replace(
                    r#"<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>"#,
                    "",
                ),
                "xl/_rels/workbook.xml.rels" => strict(&text).replace(
                    &format!(
                        r#"<Relationship Id="rId3" Type="{}/sharedStrings" Target="sharedStrings.xml"/>"#,
                        STRICT.rels
                    ),
                    "",
                ),
                "_rels/.rels" => strict(&text).replace(
                    "</Relationships>",
                    &format!("{APP_REL}</Relationships>"),
                ),
                "xl/worksheets/sheet1.xml" => strict(&text).replace(
                    "<sheetData/>",
                    r#"<sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>hello</t></is></c></row></sheetData>"#,
                ),
                _ => strict(&text),
            };
            parts.push((name.clone(), text.into_bytes()));
        }
        parts.push(("docProps/app.xml".into(), APP_XML.as_bytes().to_vec()));
        write_zip(&parts)
    }

    fn text(pkg: &SheetPackage, name: &str) -> String {
        String::from_utf8_lossy(pkg.part(name).unwrap_or_else(|| panic!("{name} missing")))
            .into_owned()
    }

    /// Every part the writer created or changed, i.e. all but the docProps
    /// part and relationship the fixture deliberately keeps transitional.
    fn assert_strict_throughout(pkg: &SheetPackage) {
        for (name, bytes) in &pkg.parts {
            if name == "docProps/app.xml" {
                continue;
            }
            let xml = String::from_utf8_lossy(bytes).replace(APP_REL, "");
            for uri in TRANSITIONAL_URIS {
                assert!(!xml.contains(uri), "{name} has {uri}: {xml}");
            }
        }
    }

    fn cell_text(pkg: &SheetPackage, sheet: usize, row: u32, col: u32) -> String {
        match pkg.workbook.sheets[sheet].cell(row, col).map(|c| &c.value) {
            Some(CellValue::Text(s)) => s.clone(),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn a_strict_workbook_is_detected_and_a_transitional_one_is_not() {
        assert!(load_xlsx(&strict_xlsx()).unwrap().strict);
        assert!(!load_xlsx(&save_xlsx(&new_xlsx())).unwrap().strict);
    }

    #[test]
    fn a_new_shared_strings_part_is_strict_and_keeps_user_text() {
        // The #602 repro: inline strings only, so save has to create the
        // shared-strings part. One cell's text IS the transitional URI.
        let mut pkg = load_xlsx(&strict_xlsx()).unwrap();
        pkg.workbook.sheets[0].set_cell(0, 1, Cell::text(T_SML));
        let out = load_xlsx(&save_xlsx(&pkg)).unwrap();

        let sst = text(&out, "xl/sharedStrings.xml");
        assert!(
            sst.contains(&format!(r#"<sst xmlns="{}""#, STRICT.sml)),
            "{sst}"
        );
        let rels = text(&out, "xl/_rels/workbook.xml.rels");
        assert!(
            rels.contains(&format!(r#"Type="{}/sharedStrings""#, STRICT.rels)),
            "{rels}"
        );
        // The user's text is data, not a namespace: it is kept as typed.
        assert_eq!(cell_text(&out, 0, 0, 0), "hello");
        assert_eq!(cell_text(&out, 0, 0, 1), T_SML);
        assert_eq!(sst.matches(T_SML).count(), 1, "{sst}");
        let without_text = {
            let mut o = out.clone();
            o.set_part("xl/sharedStrings.xml", sst.replace(T_SML, "").into_bytes());
            o
        };
        assert_strict_throughout(&without_text);
    }

    #[test]
    fn every_part_the_writer_mints_in_a_strict_workbook_is_strict() {
        let mut pkg = load_xlsx(&strict_xlsx()).unwrap();
        let rows: [[&str; 2]; 3] = [["Region", "Sales"], ["East", "10"], ["West", "30"]];
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let cell = match v.parse::<f64>() {
                    Ok(n) => Cell::number(n),
                    Err(_) => Cell::text(v),
                };
                pkg.workbook.sheets[0].set_cell(r as u32, c as u32, cell);
            }
        }
        pkg.add_table(0, (0, 0, 2, 1), true, "TableStyleMedium2")
            .expect("table");
        pkg.add_chart(
            0,
            (0, 3),
            (10, 8),
            &crate::sheet::ChartData {
                title: "Sales".into(),
                kind: "column".into(),
                categories: vec!["East".into(), "West".into()],
                series: vec![crate::sheet::ChartSeries {
                    name: "Sales".into(),
                    values: vec![10.0, 30.0],
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        pkg.set_comment(0, 1, 1, "Reviewer", "check");
        // add_sheet and add_pivot, the way the app creates a pivot.
        let frame = crate::frame::Frame::from_range(&pkg.workbook, 0, (0, 0, 2, 1));
        let spec = crate::frame::pivot_spec_from_names(
            &frame,
            &["Region".to_string()],
            &[],
            &[("Sales".to_string(), crate::frame::Agg::Sum)],
        )
        .expect("spec");
        pkg.create_pivot(
            crate::pivot::PivotSource::Range {
                sheet: "Sheet1".into(),
                rect: (0, 0, 2, 1),
            },
            &frame,
            &spec,
            "Pivot1",
        )
        .expect("pivot");
        let mut out = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_strict_throughout(&out);
        assert!(out.strict);
        assert_eq!(out.workbook.tables.len(), 1);
        assert_eq!(out.workbook.pivots.len(), 1);
        assert_eq!(out.workbook.sheets.len(), 2);
        assert_eq!(out.comments().len(), 1);

        // An edited chart is regenerated from the model: still strict.
        let chart_part = {
            let dw = out.workbook.sheets[0]
                .drawings
                .iter_mut()
                .find(|d| matches!(d.kind, crate::sheet::DrawingKind::Chart(_)))
                .expect("the chart reloads");
            let crate::sheet::DrawingKind::Chart(cd) = &mut dw.kind else {
                unreachable!()
            };
            cd.title = "Edited".into();
            cd.edited = true;
            cd.part.clone().expect("chart part")
        };
        let again = load_xlsx(&save_xlsx(&out)).unwrap();
        let chart = text(&again, &chart_part);
        assert!(chart.contains("Edited"), "the edit was written: {chart}");
        assert!(chart.contains(STRICT.chart), "{chart}");
        assert_strict_throughout(&again);
    }

    #[test]
    fn a_strict_workbooks_untouched_transitional_parts_keep_their_bytes() {
        let input = load_xlsx(&strict_xlsx()).unwrap();
        let out = load_xlsx(&save_xlsx(&input)).unwrap();
        for name in ["_rels/.rels", "docProps/app.xml", "xl/styles.xml"] {
            assert_eq!(out.part(name), input.part(name), "{name} changed");
        }
        assert!(text(&out, "_rels/.rels").contains(APP_REL));
    }

    #[test]
    fn a_transitional_save_mints_nothing_strict() {
        let mut pkg = load_xlsx(&save_xlsx(&new_xlsx())).unwrap();
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::text("a"));
        pkg.workbook.sheets[0].set_cell(1, 0, Cell::number(1.0));
        pkg.add_table(0, (0, 0, 1, 0), true, "TableStyleMedium2")
            .expect("table");
        pkg.add_chart(
            0,
            (0, 3),
            (10, 8),
            &crate::sheet::ChartData {
                kind: "column".into(),
                categories: vec!["a".into()],
                series: vec![crate::sheet::ChartSeries {
                    name: "s".into(),
                    values: vec![1.0],
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        pkg.set_comment(0, 0, 0, "A", "note");
        let out = load_xlsx(&save_xlsx(&pkg)).unwrap();
        for (name, bytes) in &out.parts {
            let xml = String::from_utf8_lossy(bytes);
            assert!(!xml.contains("purl.oclc.org"), "{name}: {xml}");
        }
    }
}

/// Children the writer adds to workbook.xml land at their CT_Workbook
/// position, so Excel opens the file without a repair (#773).
#[cfg(test)]
mod ct_workbook_order_tests {
    use super::*;

    const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const WB: &str = "xl/workbook.xml";

    /// A new workbook with a Region/Sales range on Sheet1, and its
    /// workbook.xml replaced by `workbook` when given.
    fn with_data(workbook: Option<String>) -> SheetPackage {
        let mut pkg = new_xlsx();
        if let Some(wb) = workbook {
            pkg.set_part(WB, wb.into_bytes());
        }
        let rows = [("Region", None), ("East", Some(10.0)), ("West", Some(30.0))];
        for (r, (region, sales)) in rows.iter().enumerate() {
            let r = r as u32;
            pkg.workbook.sheets[0].set_cell(r, 0, Cell::text(region));
            let sales = sales.map_or_else(|| Cell::text("Sales"), Cell::number);
            pkg.workbook.sheets[0].set_cell(r, 1, sales);
        }
        pkg
    }

    fn pivot(pkg: &mut SheetPackage) {
        add_default_pivot(pkg).expect("add_pivot");
    }

    /// A Region/Sales pivot from Sheet1!A1:B3, placed at A6.
    fn add_default_pivot(pkg: &mut SheetPackage) -> Option<usize> {
        let measure = crate::pivot::DataField {
            name: "Sum of Sales".into(),
            field: 1,
            agg: crate::frame::Agg::Sum,
        };
        pkg.add_pivot(
            crate::pivot::PivotSource::Range {
                sheet: "Sheet1".into(),
                rect: (0, 0, 2, 1),
            },
            vec!["Region".into(), "Sales".into()],
            measure,
            0,
            (5, 0),
        )
    }

    fn part(pkg: &SheetPackage, name: &str) -> String {
        String::from_utf8(pkg.part(name).unwrap().to_vec()).unwrap()
    }

    /// `a` stands before `b` in `xml`.
    fn before(xml: &str, a: &str, b: &str) -> bool {
        match (xml.find(a), xml.find(b)) {
            (Some(i), Some(j)) => i < j,
            _ => false,
        }
    }

    fn workbook(inner: &str) -> String {
        format!(r#"<?xml version="1.0"?><workbook xmlns="{NS}" xmlns:r="{R}">{inner}</workbook>"#)
    }

    const SHEETS: &str = r#"<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>"#;

    #[test]
    fn add_pivot_puts_pivot_caches_after_defined_names_and_calc_pr() {
        let names =
            r#"<definedNames><definedName name="Total">Sheet1!$B$2</definedName></definedNames>"#;
        let calc = r#"<calcPr calcId="191029"/>"#;
        let mut pkg = with_data(Some(workbook(&format!("{SHEETS}{names}{calc}"))));
        pivot(&mut pkg);
        let wb = part(&pkg, WB);
        assert!(
            wb.contains(&format!("{calc}<pivotCaches><pivotCache cacheId=")),
            "{wb}"
        );
        assert!(before(&wb, "</pivotCaches>", "</workbook>"), "{wb}");
    }

    #[test]
    fn add_pivot_puts_pivot_caches_between_custom_workbook_views_and_ext_lst() {
        let views = r#"<customWorkbookViews><customWorkbookView name="Mine" guid="{00000000-0000-0000-0000-000000000001}" windowWidth="800" windowHeight="600" activeSheetId="1"/></customWorkbookViews>"#;
        // An unknown child (x15ac:absPath's wrapper) is not an anchor.
        let alt = r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><mc:Choice Requires="x15"/></mc:AlternateContent>"#;
        let tail = r#"<fileRecoveryPr repairLoad="1"/><extLst><ext uri="{x}"/></extLst>"#;
        let mut pkg = with_data(Some(workbook(&format!(
            r#"{alt}{SHEETS}<calcPr calcId="1"/>{views}{tail}"#
        ))));
        pivot(&mut pkg);
        let wb = part(&pkg, WB);
        assert!(wb.contains(&format!("{views}<pivotCaches>")), "{wb}");
        assert!(wb.contains(&format!("</pivotCaches>{tail}")), "{wb}");
    }

    #[test]
    fn add_pivot_prefixes_pivot_caches_on_a_prefixed_root() {
        let wb = format!(
            r#"<?xml version="1.0"?><x:workbook xmlns:x="{NS}" xmlns:r="{R}"><x:sheets><x:sheet name="Sheet1" sheetId="1" r:id="rId1"/></x:sheets><x:calcPr calcId="1"/><x:extLst/></x:workbook>"#
        );
        let mut pkg = with_data(Some(wb));
        pivot(&mut pkg);
        let wb = part(&pkg, WB);
        assert!(
            wb.contains(r#"<x:calcPr calcId="1"/><x:pivotCaches><x:pivotCache cacheId="#),
            "{wb}"
        );
        assert!(wb.contains("</x:pivotCaches><x:extLst/>"), "{wb}");
        assert!(!wb.contains("<pivotCache"), "{wb}");
    }

    #[test]
    fn add_pivot_appends_to_an_existing_pivot_caches() {
        let caches = r#"<pivotCaches><pivotCache cacheId="7" r:id="rId9"/></pivotCaches>"#;
        let mut pkg = with_data(Some(workbook(&format!("{SHEETS}{caches}"))));
        pivot(&mut pkg);
        let wb = part(&pkg, WB);
        assert_eq!(wb.matches("<pivotCaches>").count(), 1, "{wb}");
        assert!(
            wb.contains(r#"<pivotCaches><pivotCache cacheId="7" r:id="rId9"/><pivotCache "#),
            "{wb}"
        );
    }

    #[test]
    fn a_new_workbooks_pivot_and_formula_save_calc_pr_before_pivot_caches() {
        // A new workbook has no <calcPr>: save adds one for the formula,
        // and it must not land after the pivot's <pivotCaches>.
        let mut pkg = with_data(None);
        pkg.workbook.sheets[0].set_cell(3, 1, Cell::formula("SUM(B2:B3)"));
        pivot(&mut pkg);
        let saved = load_xlsx(&save_xlsx(&pkg)).expect("reload");
        let wb = part(&saved, WB);
        assert!(before(&wb, "<calcPr", "<pivotCaches>"), "{wb}");
        assert_eq!(saved.workbook.pivots.len(), 1);
    }

    #[test]
    fn ensure_full_calc_puts_calc_pr_before_ext_lst() {
        let names =
            r#"<definedNames><definedName name="T">Sheet1!$A$1</definedName></definedNames>"#;
        let out = ensure_full_calc(&workbook(&format!("{SHEETS}{names}<extLst/>")));
        assert!(
            out.contains(&format!(
                r#"{names}<calcPr calcId="0" fullCalcOnLoad="1"/><extLst/>"#
            )),
            "{out}"
        );

        let prefixed =
            format!(r#"<x:workbook xmlns:x="{NS}"><x:sheets/><x:pivotCaches/></x:workbook>"#);
        let out = ensure_full_calc(&prefixed);
        assert!(
            out.contains(r#"<x:sheets/><x:calcPr calcId="0" fullCalcOnLoad="1"/><x:pivotCaches/>"#),
            "{out}"
        );
    }

    /// Top-level `<…tag>` elements in `xml`, in any prefix.
    fn count_local(xml: &str, tag: &str) -> usize {
        let mut p = XmlParser::new(xml);
        let mut n = 0;
        loop {
            match p.next() {
                Event::Start if local(p.name()) == tag => n += 1,
                Event::Eof => return n,
                _ => {}
            }
        }
    }

    fn prefixed(inner: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><x:workbook xmlns:x="{NS}" xmlns:r="{R}"><x:sheets><x:sheet name="Sheet1" sheetId="1" r:id="rId1"/></x:sheets>{inner}</x:workbook>"#
        )
    }

    #[test]
    fn a_prefixed_calc_pr_is_not_added_twice_at_save() {
        let mut pkg = with_data(Some(prefixed(r#"<x:calcPr calcId="1"/>"#)));
        pkg.workbook.sheets[0].set_cell(3, 1, Cell::formula("SUM(B2:B3)"));
        let saved = load_xlsx(&save_xlsx(&pkg)).expect("reload");
        let wb = part(&saved, WB);
        assert_eq!(count_local(&wb, "calcPr"), 1, "{wb}");
    }

    #[test]
    fn add_pivot_does_not_add_a_second_prefixed_pivot_caches() {
        let caches = r#"<x:pivotCaches><x:pivotCache cacheId="7" r:id="rId9"/></x:pivotCaches>"#;
        let mut pkg = with_data(Some(prefixed(caches)));
        pivot(&mut pkg);
        let wb = part(&pkg, WB);
        assert_eq!(count_local(&wb, "pivotCaches"), 1, "{wb}");
        // And the cache is registered in it, in its prefix.
        assert_eq!(count_local(&wb, "pivotCache"), 2, "{wb}");
        assert!(
            wb.contains(
                r#"<x:pivotCache cacheId="7" r:id="rId9"/><x:pivotCache cacheId="8" r:id="#
            ),
            "{wb}"
        );
        assert!(wb.contains("</x:pivotCaches></x:workbook>"), "{wb}");
    }

    #[test]
    fn workbook_slot_is_present_for_a_tag_already_there() {
        let wb = prefixed(r#"<x:calcPr calcId="1"/><x:extLst/>"#);
        assert_eq!(workbook_slot(&wb, "calcPr"), WorkbookSlot::Present);
        // Even out of order, past the place it would go.
        let wb = prefixed(r#"<x:extLst/><x:calcPr calcId="1"/>"#);
        assert_eq!(workbook_slot(&wb, "calcPr"), WorkbookSlot::Present);
        assert!(matches!(
            workbook_slot(&wb, "pivotCaches"),
            WorkbookSlot::At(_, _)
        ));
    }

    #[test]
    fn a_prefixed_calc_pr_under_an_unprefixed_root_is_not_added_twice() {
        let wb = workbook(&format!(r#"{SHEETS}<x:calcPr xmlns:x="{NS}" calcId="1"/>"#));
        let mut pkg = with_data(Some(wb));
        pkg.workbook.sheets[0].set_cell(3, 1, Cell::formula("SUM(B2:B3)"));
        let saved = load_xlsx(&save_xlsx(&pkg)).expect("reload");
        let wb = part(&saved, WB);
        assert_eq!(count_local(&wb, "calcPr"), 1, "{wb}");
    }

    #[test]
    fn add_pivot_does_not_add_a_second_self_closing_pivot_caches() {
        let mut pkg = with_data(Some(workbook(&format!("{SHEETS}<pivotCaches/>"))));
        pivot(&mut pkg);
        let wb = part(&pkg, WB);
        assert_eq!(count_local(&wb, "pivotCaches"), 1, "{wb}");
        // Opened up to hold the entry.
        assert!(
            wb.contains(r#"<pivotCaches><pivotCache cacheId="1" r:id="#),
            "{wb}"
        );
        assert!(wb.contains("/></pivotCaches></workbook>"), "{wb}");

        let mut pkg = with_data(Some(prefixed(r#"<x:pivotCaches />"#)));
        pivot(&mut pkg);
        let wb = part(&pkg, WB);
        assert_eq!(count_local(&wb, "pivotCaches"), 1, "{wb}");
        assert!(
            wb.contains(r#"<x:pivotCaches ><x:pivotCache cacheId="1" r:id="#),
            "{wb}"
        );
        assert!(wb.contains("/></x:pivotCaches></x:workbook>"), "{wb}");
    }

    /// The namespace `r:` means on the first `<…pivotCache>` with `cacheId`.
    fn r_of_new_entry(xml: &str, cache_id: &str) -> Option<String> {
        let mut p = XmlParser::new(xml);
        loop {
            match p.next() {
                Event::Start
                    if local(p.name()) == "pivotCache" && p.attr("cacheId") == cache_id =>
                {
                    return p
                        .namespace_attrs()
                        .iter()
                        .rev()
                        .find(|a| a.name == "xmlns:r")
                        .map(|a| a.value.to_string());
                }
                Event::Eof => return None,
                _ => {}
            }
        }
    }

    #[test]
    fn add_pivot_binds_r_on_a_root_without_xmlns_r() {
        let wb = format!(
            r#"<?xml version="1.0"?><x:workbook xmlns:x="{NS}"><x:sheets><x:sheet name="Sheet1" sheetId="1" xmlns:r="{R}" r:id="rId1"/></x:sheets><x:calcPr calcId="1"/></x:workbook>"#
        );
        let mut pkg = with_data(Some(wb));
        pivot(&mut pkg);
        let wb = part(&pkg, WB);
        assert_eq!(r_of_new_entry(&wb, "1").as_deref(), Some(R), "{wb}");
        assert!(
            wb.contains(&format!(r#"<x:workbook xmlns:x="{NS}" xmlns:r="{R}">"#)),
            "declared on the root: {wb}"
        );
        assert_eq!(count_local(&wb, "pivotCache"), 1, "{wb}");
    }

    #[test]
    fn add_pivot_binds_r_when_root_maps_r_elsewhere() {
        let wb = format!(
            r#"<?xml version="1.0"?><workbook xmlns="{NS}" xmlns:r="urn:other"><sheets><sheet name="Sheet1" sheetId="1" xmlns:r="{R}" r:id="rId1"/></sheets><pivotCaches><pivotCache cacheId="3" xmlns:r="{R}" r:id="rId7"/></pivotCaches></workbook>"#
        );
        let mut pkg = with_data(Some(wb));
        pivot(&mut pkg);
        let wb = part(&pkg, WB);
        assert_eq!(r_of_new_entry(&wb, "4").as_deref(), Some(R), "{wb}");
        // The root keeps its own binding.
        assert!(wb.contains(r#"xmlns:r="urn:other">"#), "{wb}");
        assert_eq!(wb.matches(r#"xmlns:r="urn:other""#).count(), 1, "{wb}");
    }

    #[test]
    fn add_pivot_moves_a_misplaced_pivot_caches() {
        // Where an older docxy put it: right after </sheets>, ahead of
        // definedNames and calcPr.
        let caches = r#"<pivotCaches><pivotCache cacheId="7" r:id="rId9"/></pivotCaches>"#;
        let names =
            r#"<definedNames><definedName name="T">Sheet1!$A$1</definedName></definedNames>"#;
        let calc = r#"<calcPr calcId="1"/>"#;
        let mut pkg = with_data(Some(workbook(&format!(
            "{SHEETS}{caches}{names}{calc}<extLst/>"
        ))));
        pivot(&mut pkg);
        let wb = part(&pkg, WB);
        assert_eq!(count_local(&wb, "pivotCaches"), 1, "{wb}");
        assert!(
            wb.contains(&format!(
                r#"{SHEETS}{names}{calc}<pivotCaches><pivotCache cacheId="7" r:id="rId9"/><pivotCache cacheId="8" r:id="#
            )),
            "{wb}"
        );
        assert!(wb.contains("</pivotCaches><extLst/>"), "{wb}");
    }

    #[test]
    fn add_pivot_leaves_a_well_placed_pivot_caches_where_it_is() {
        // An unknown child between it and extLst is no reason to move it.
        let alt = r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"/>"#;
        let caches = r#"<pivotCaches><pivotCache cacheId="7" r:id="rId9"/></pivotCaches>"#;
        let mut pkg = with_data(Some(workbook(&format!("{SHEETS}{caches}{alt}<extLst/>"))));
        pivot(&mut pkg);
        let wb = part(&pkg, WB);
        assert!(
            wb.contains(&format!("</pivotCaches>{alt}<extLst/>")),
            "{wb}"
        );
    }

    #[test]
    fn add_pivot_refuses_a_broken_workbook_rels_part() {
        let mut pkg = with_data(None);
        let rels = part(&pkg, "xl/_rels/workbook.xml.rels");
        let cut = rels.rfind("</Relationships>").unwrap();
        pkg.set_part(
            "xl/_rels/workbook.xml.rels",
            rels.as_bytes()[..cut].to_vec(),
        );
        let parts = pkg.parts.clone();
        assert!(add_default_pivot(&mut pkg).is_none());
        assert_eq!(pkg.parts, parts);
        assert!(pkg.workbook.pivots.is_empty());
    }

    #[test]
    fn add_pivot_refuses_a_broken_destination_sheet_rels_part() {
        let mut pkg = with_data(None);
        pkg.set_part(
            "xl/worksheets/_rels/sheet1.xml.rels",
            format!(r#"<Relationships xmlns="{R}"/>"#).into_bytes(),
        );
        let parts = pkg.parts.clone();
        assert!(add_default_pivot(&mut pkg).is_none());
        assert_eq!(pkg.parts, parts);
        assert!(pkg.workbook.pivots.is_empty());
    }

    #[test]
    fn ensure_full_calc_marks_a_prefixed_calc_pr() {
        let out = ensure_full_calc(&prefixed(r#"<x:calcPr calcId="1"/>"#));
        assert!(
            out.contains(r#"<x:calcPr calcId="1" fullCalcOnLoad="1"/>"#),
            "{out}"
        );
        assert_eq!(count_local(&out, "calcPr"), 1, "{out}");

        // Open/close, under an unprefixed root.
        let out = ensure_full_calc(&workbook(&format!(
            r#"{SHEETS}<x:calcPr xmlns:x="{NS}" calcId="1"></x:calcPr>"#
        )));
        assert!(
            out.contains(&format!(
                r#"<x:calcPr xmlns:x="{NS}" calcId="1" fullCalcOnLoad="1"></x:calcPr>"#
            )),
            "{out}"
        );
        assert_eq!(count_local(&out, "calcPr"), 1, "{out}");
    }

    #[test]
    fn ensure_full_calc_leaves_a_cut_off_calc_pr_alone() {
        let wb = format!(
            r#"<?xml version="1.0"?><workbook xmlns="{NS}" xmlns:r="{R}">{SHEETS}<calcPr calcId="1" x="é"#
        );
        assert_eq!(ensure_full_calc(&wb), wb);
        let wb = format!("{wb}é");
        assert_eq!(ensure_full_calc(&wb), wb);
    }

    #[test]
    fn remove_pivot_takes_its_entry_out_of_a_prefixed_pivot_caches() {
        let caches = r#"<x:pivotCaches><x:pivotCache cacheId="7" r:id="rId9"/></x:pivotCaches>"#;
        let mut pkg = with_data(Some(prefixed(caches)));
        let idx = add_default_pivot(&mut pkg).expect("add_pivot");
        assert_eq!(count_local(&part(&pkg, WB), "pivotCache"), 2);
        assert!(pkg.remove_pivot(idx));
        let wb = part(&pkg, WB);
        assert!(wb.contains(caches), "{wb}");
        assert_eq!(count_local(&wb, "pivotCache"), 1, "{wb}");
    }

    #[test]
    fn remove_pivot_takes_out_a_wrapper_it_opened() {
        for caches in [r#"<pivotCaches />"#, r#"<x:pivotCaches/>"#] {
            let inner = if caches.starts_with("<x:") {
                prefixed(caches)
            } else {
                workbook(&format!("{SHEETS}{caches}"))
            };
            let mut pkg = with_data(Some(inner));
            let idx = add_default_pivot(&mut pkg).expect("add_pivot");
            assert!(pkg.remove_pivot(idx));
            let wb = part(&pkg, WB);
            assert_eq!(count_local(&wb, "pivotCache"), 0, "{wb}");
            assert_eq!(count_local(&wb, "pivotCaches"), 0, "{wb}");
        }
    }

    #[test]
    fn ensure_full_calc_keeps_a_prefixed_full_calc() {
        for v in ["1", "true"] {
            let wb = prefixed(&format!(r#"<x:calcPr calcId="1" fullCalcOnLoad="{v}"/>"#));
            assert_eq!(ensure_full_calc(&wb), wb);
        }
    }

    #[test]
    fn ensure_full_calc_turns_on_a_false_full_calc() {
        for v in ["0", "false"] {
            let wb = prefixed(&format!(r#"<x:calcPr fullCalcOnLoad="{v}" calcId="1"/>"#));
            let out = ensure_full_calc(&wb);
            assert!(
                out.contains(r#"<x:calcPr fullCalcOnLoad="1" calcId="1"/>"#),
                "{out}"
            );
        }
        let wb = workbook(&format!(
            r#"{SHEETS}<calcPr calcId="1" fullCalcOnLoad='0'/>"#
        ));
        let out = ensure_full_calc(&wb);
        assert!(
            out.contains(r#"<calcPr calcId="1" fullCalcOnLoad='1'/>"#),
            "{out}"
        );
    }

    #[test]
    fn workbook_slot_is_unknown_in_a_truncated_part() {
        assert_eq!(
            workbook_slot(r#"<workbook><sheets>"#, "calcPr"),
            WorkbookSlot::Unknown
        );
        assert_eq!(
            workbook_slot(r#"<workbook/>"#, "calcPr"),
            WorkbookSlot::Unknown
        );
        // With no root end tag, the calcPr append has nothing to go before.
        let out = ensure_full_calc("<workbook><sheets/>");
        assert_eq!(out, "<workbook><sheets/>");
    }
}

/// Conditional formatting and data validation follow structural edits and
/// sheet renames, and a save writes the moved elements back (#822).
#[cfg(test)]
mod rule_shift_tests {
    use super::print_setup_tests::book;
    use super::*;
    use crate::edit::{delete_cols, delete_rows, insert_cols, insert_rows, rename_sheet};
    use crate::sheet::{CfKind, CondFormat, Dxf};

    const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    const SHEET1: &str = "xl/worksheets/sheet1.xml";
    const SHEET2: &str = "xl/worksheets/sheet2.xml";
    const DATA: &str = r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>"#;
    const MARGINS: &str = r#"<pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>"#;

    /// One sheet named `name` whose body after `<sheetData>` is `rest`.
    fn one(name: &str, rest: &str) -> SheetPackage {
        let body = format!("{DATA}{rest}");
        load_xlsx(&book("", &[(name, Some(&body))])).expect("fixture loads")
    }

    fn part(pkg: &SheetPackage, name: &str) -> String {
        String::from_utf8_lossy(pkg.part(name).expect("part present")).into_owned()
    }

    /// Save and reload; the reloaded package and the saved text of `name`.
    fn saved(pkg: &SheetPackage, name: &str) -> (SheetPackage, String) {
        let re = load_xlsx(&save_xlsx(pkg)).expect("saved file reloads");
        let xml = part(&re, name);
        (re, xml)
    }

    fn count(xml: &str, needle: &str) -> usize {
        xml.matches(needle).count()
    }

    /// The text of a rule's first formula.
    fn first_formula(cf: &CondFormat) -> String {
        cf.rules[0].formulas()[0].clone()
    }

    const DV_RULE: &str = r#"<dataValidations count="1"><dataValidation type="whole" operator="between" errorStyle="warning" allowBlank="1" showErrorMessage="1" errorTitle="Score" error="10 to 90 only" promptTitle="P" prompt="pick" xr:uid="{1}" sqref="B2:B10"><formula1>10</formula1><formula2>90</formula2></dataValidation></dataValidations>"#;

    #[test]
    fn dv_reads_every_attribute() {
        let pkg = one("S", DV_RULE);
        let dv = &pkg.workbook.sheets[0].validations[0];
        assert_eq!(dv.error_style, crate::sheet::AlertStyle::Warning);
        assert!(dv.allow_blank && dv.show_error && !dv.show_input);
        assert!(dv.show_dropdown);
        assert_eq!(
            (dv.error_title.as_str(), dv.error.as_str()),
            ("Score", "10 to 90 only")
        );
        assert_eq!(dv.prompt_title, "P");
        assert_eq!(dv.prompt.as_deref(), Some("pick"));
        // showDropDown="1" hides the in-cell dropdown.
        let hidden = one(
            "S",
            r#"<dataValidations count="1"><dataValidation type="list" showDropDown="1" sqref="A1"><formula1>"a,b"</formula1></dataValidation></dataValidations>"#,
        );
        assert!(!hidden.workbook.sheets[0].validations[0].show_dropdown);
    }

    #[test]
    fn dv_single_quoted_attributes_edited_with_awkward_text_stay_well_formed() {
        let rule = r#"<dataValidations count="1"><dataValidation type="whole" operator="between" errorTitle='old t' error='old' promptTitle='old p' prompt='old m' showErrorMessage="1" sqref="B2:B10"><formula1>10</formula1><formula2>90</formula2></dataValidation></dataValidations>"#;
        for text in [
            "Can't enter that",
            "say \"no\"",
            "a & b",
            "1 < 2 > 0",
            "it's \"both\" & <more>",
        ] {
            let mut pkg = one("S", rule);
            {
                let dv = &mut pkg.workbook.sheets[0].validations[0];
                dv.error = text.into();
                dv.error_title = text.into();
                dv.prompt = Some(text.into());
                dv.prompt_title = text.into();
            }
            let (re, ws) = saved(&pkg, SHEET1);
            let dv = &re.workbook.sheets[0].validations[0];
            assert_eq!(
                (
                    dv.error.as_str(),
                    dv.error_title.as_str(),
                    dv.prompt.as_deref(),
                    dv.prompt_title.as_str()
                ),
                (text, text, Some(text), text),
                "{text}: {ws}"
            );
        }
    }

    #[test]
    fn dv_cdata_formulas_edited_round_trip() {
        let rule = r#"<dataValidations count="1"><dataValidation type="whole" operator="between" sqref="B2:B10"><formula1><![CDATA[10]]></formula1><formula2><![CDATA[90]]></formula2></dataValidation></dataValidations>"#;
        let mut pkg = one("S", rule);
        assert_eq!(pkg.workbook.sheets[0].validations[0].formula1, "10");
        {
            let dv = &mut pkg.workbook.sheets[0].validations[0];
            dv.formula1 = "20".into();
            dv.formula2 = "80".into();
        }
        let (re, ws) = saved(&pkg, SHEET1);
        let dv = &re.workbook.sheets[0].validations[0];
        assert_eq!(
            (dv.formula1.as_str(), dv.formula2.as_str()),
            ("20", "80"),
            "{ws}"
        );
    }

    #[test]
    fn dv_many_new_rules_round_trip_in_order_with_a_right_count() {
        let mut pkg = one("S", DV_RULE);
        let n = 300;
        for r in 0..n {
            pkg.workbook.sheets[0]
                .validations
                .push(crate::sheet::DataValidation {
                    ranges: vec![(20 + r, 0, 20 + r, 0)],
                    kind: "whole".into(),
                    operator: "equal".into(),
                    formula1: r.to_string(),
                    show_error: true,
                    ..Default::default()
                });
        }
        let (re, ws) = saved(&pkg, SHEET1);
        assert!(
            ws.contains(&format!(r#"<dataValidations count="{}">"#, n + 1)),
            "count"
        );
        let rules = &re.workbook.sheets[0].validations;
        assert_eq!(rules.len(), n as usize + 1);
        let got: Vec<&str> = rules[1..].iter().map(|d| d.formula1.as_str()).collect();
        let want: Vec<String> = (0..n).map(|r| r.to_string()).collect();
        assert_eq!(got, want.iter().map(String::as_str).collect::<Vec<_>>());
        // And onto a sheet with no block at all.
        let mut pkg = one("S", "");
        pkg.workbook.sheets[0]
            .validations
            .push(crate::sheet::DataValidation {
                ranges: vec![(0, 0, 0, 0)],
                kind: "whole".into(),
                formula1: "1".into(),
                show_error: true,
                ..Default::default()
            });
        pkg.workbook.sheets[0]
            .validations
            .push(crate::sheet::DataValidation {
                ranges: vec![(1, 0, 1, 0)],
                kind: "whole".into(),
                formula1: "2".into(),
                show_error: true,
                ..Default::default()
            });
        let (re, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(r#"<dataValidations count="2">"#), "{ws}");
        assert_eq!(re.workbook.sheets[0].validations.len(), 2);
    }

    #[test]
    fn dv_untouched_element_is_byte_identical() {
        let pkg = one("S", DV_RULE);
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(DV_RULE), "{ws}");
    }

    #[test]
    fn dv_edited_rule_rewrites_changed_attributes_only() {
        let mut pkg = one("S", DV_RULE);
        {
            let dv = &mut pkg.workbook.sheets[0].validations[0];
            dv.error_style = crate::sheet::AlertStyle::Stop;
            dv.error = "no".into();
            dv.show_input = true;
            dv.operator = "greaterThan".into();
            dv.formula2.clear();
        }
        let (re, ws) = saved(&pkg, SHEET1);
        for kept in [
            r#"xr:uid="{1}""#,
            r#"errorTitle="Score""#,
            r#"promptTitle="P""#,
            r#"operator="greaterThan""#,
            r#"showInputMessage="1""#,
            r#"error="no""#,
            r#"<formula1>10</formula1></dataValidation>"#,
        ] {
            assert!(ws.contains(kept), "{kept} in {ws}");
        }
        assert!(
            !ws.contains("errorStyle") && !ws.contains("<formula2>"),
            "{ws}"
        );
        let dv = &re.workbook.sheets[0].validations[0];
        assert_eq!(dv.error, "no");
        assert!(dv.show_input);
        assert_eq!(dv.formula2, "");
        assert_eq!(dv.error_style, crate::sheet::AlertStyle::Stop);
    }

    #[test]
    fn dv_new_rule_is_appended_and_a_cleared_one_goes() {
        let mut pkg = one("S", DV_RULE);
        let s = &mut pkg.workbook.sheets[0];
        s.validations.push(crate::sheet::DataValidation {
            ranges: vec![(0, 0, 0, 0)],
            kind: "list".into(),
            formula1: "\"a,b\"".into(),
            show_error: true,
            error_title: "T".into(),
            ..Default::default()
        });
        let (re, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(r#"<dataValidations count="2">"#), "{ws}");
        let s = &re.workbook.sheets[0];
        assert_eq!(s.validations.len(), 2);
        assert_eq!(s.validations[1].error_title, "T");
        assert!(s.validations[1].show_error);

        // Clear the original: its element goes, the new one stays.
        let mut pkg = re;
        let s = &mut pkg.workbook.sheets[0];
        let gone = s.validations.remove(0);
        s.dv_removed.extend(gone.ix);
        let (re, ws) = saved(&pkg, SHEET1);
        assert_eq!(count(&ws, "<dataValidation "), 1, "{ws}");
        assert_eq!(re.workbook.sheets[0].validations.len(), 1);
    }

    #[test]
    fn dv_new_rule_on_a_sheet_without_a_block() {
        let mut pkg = one("S", "");
        pkg.workbook.sheets[0]
            .validations
            .push(crate::sheet::DataValidation {
                ranges: vec![(1, 1, 3, 1)],
                kind: "whole".into(),
                operator: "between".into(),
                formula1: "1".into(),
                formula2: "5".into(),
                allow_blank: true,
                show_error: true,
                ..Default::default()
            });
        let (re, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(r#"sqref="B2:B4""#), "{ws}");
        let dv = &re.workbook.sheets[0].validations[0];
        assert_eq!((dv.formula1.as_str(), dv.formula2.as_str()), ("1", "5"));
        assert!(dv.allow_blank && dv.show_error);
    }

    #[test]
    fn dv_paste_over_a_validated_cell_saves_the_split_sqref() {
        let mut pkg = one("S", DV_RULE);
        let s = &mut pkg.workbook.sheets[0];
        // Copy H2 (no rule), paste at B6.
        crate::validation::paste_rules(s, &[], (1, 7, 1, 7), (5, 1), (1, 1));
        let (re, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(r#"sqref="B2:B5 B7:B10""#), "{ws}");
        assert_eq!(
            re.workbook.sheets[0].validations[0].ranges,
            vec![(1, 1, 4, 1), (6, 1, 9, 1)]
        );
    }

    #[test]
    fn cf_and_dv_follow_inserts_and_rename_through_save() {
        // The issue's repro.
        let mut pkg = one(
            "Report",
            r#"<conditionalFormatting sqref="A1:A5"><cfRule type="expression" dxfId="0" priority="1"><formula>_xlfn.XOR(A1,B1)</formula></cfRule></conditionalFormatting><dataValidations count="1"><dataValidation type="custom" allowBlank="1" sqref="B1:B5"><formula1>_xlfn.ISFORMULA(A1)</formula1></dataValidation></dataValidations>"#,
        );
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        insert_cols(&mut pkg.workbook, 0, 0, 1);
        rename_sheet(&mut pkg.workbook, 0, "Q1");

        let s = &pkg.workbook.sheets[0];
        assert_eq!(s.cond_formats[0].ranges, vec![(1, 1, 5, 1)]);
        assert_eq!(first_formula(&s.cond_formats[0]), "XOR(B2,C2)");
        assert_eq!(s.validations[0].ranges, vec![(1, 2, 5, 2)]);
        assert_eq!(s.validations[0].formula1, "ISFORMULA(B2)");

        let (re, ws) = saved(&pkg, SHEET1);
        assert!(
            ws.contains(r#"<conditionalFormatting sqref="B2:B6"><cfRule type="expression" dxfId="0" priority="1"><formula>_xlfn.XOR(B2,C2)</formula></cfRule></conditionalFormatting>"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<dataValidations count="1"><dataValidation type="custom" allowBlank="1" sqref="C2:C6"><formula1>_xlfn.ISFORMULA(B2)</formula1></dataValidation></dataValidations>"#),
            "{ws}"
        );
        let s = &re.workbook.sheets[0];
        assert_eq!(s.name, "Q1");
        assert_eq!(s.cond_formats[0].ranges, vec![(1, 1, 5, 1)]);
        assert_eq!(first_formula(&s.cond_formats[0]), "_xlfn.XOR(B2,C2)");
        assert_eq!(s.validations[0].ranges, vec![(1, 2, 5, 2)]);
        assert_eq!(s.validations[0].formula1, "_xlfn.ISFORMULA(B2)");
    }

    #[test]
    fn cf_and_dv_trim_and_drop_on_delete() {
        // Sheet1: a CF the delete trims and one it takes whole; two DVs, one
        // trimmed and one taken, so the block stays with a count of 1.
        // Sheet2: its only DV is taken, so the block goes.
        let s1 = format!(
            r#"{DATA}<conditionalFormatting sqref="A1:A5"><cfRule type="expression" priority="1"><formula>$A$1&lt;5</formula></cfRule></conditionalFormatting><conditionalFormatting sqref="C3:C4"><cfRule type="expression" priority="2"><formula>TRUE</formula></cfRule></conditionalFormatting><dataValidations count="2"><dataValidation type="whole" sqref="B3:B4"><formula1>1</formula1></dataValidation><dataValidation type="whole" sqref="D1:D10 E3"><formula1>2</formula1></dataValidation></dataValidations>{MARGINS}"#
        );
        let s2 = format!(
            r#"{DATA}<dataValidations count="1"><dataValidation type="whole" sqref="A2"><formula1>1</formula1></dataValidation></dataValidations>{MARGINS}"#
        );
        let mut pkg =
            load_xlsx(&book("", &[("One", Some(&s1)), ("Two", Some(&s2))])).expect("loads");
        delete_rows(&mut pkg.workbook, 0, 2, 2); // rows 3:4
        delete_rows(&mut pkg.workbook, 1, 1, 1); // row 2

        let s = &pkg.workbook.sheets[0];
        assert_eq!(s.cond_formats.len(), 1);
        assert_eq!(s.cond_formats[0].ranges, vec![(0, 0, 2, 0)]);
        assert_eq!(s.cf_removed, vec![1]);
        assert_eq!(s.validations.len(), 1);
        // E3 went with its row; D1:D10 lost two of its rows.
        assert_eq!(s.validations[0].ranges, vec![(0, 3, 7, 3)]);
        assert_eq!(s.dv_removed, vec![0]);
        assert!(pkg.workbook.sheets[1].validations.is_empty());

        let (re, ws) = saved(&pkg, SHEET1);
        assert_eq!(count(&ws, "<conditionalFormatting"), 1, "{ws}");
        // The absolute ref was not deleted: it keeps its text.
        assert!(
            ws.contains(r#"<conditionalFormatting sqref="A1:A3"><cfRule type="expression" priority="1"><formula>$A$1&lt;5</formula>"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<dataValidations count="1"><dataValidation type="whole" sqref="D1:D8"><formula1>2</formula1></dataValidation></dataValidations>"#),
            "{ws}"
        );
        assert_eq!(re.workbook.sheets[0].cond_formats.len(), 1);
        assert_eq!(re.workbook.sheets[0].validations.len(), 1);
        let ws2 = part(&re, SHEET2);
        assert!(!ws2.contains("dataValidation"), "{ws2}");
        assert!(re.workbook.sheets[1].validations.is_empty());
    }

    #[test]
    fn cf_relative_ref_survives_anchor_row_delete() {
        let mut pkg = one(
            "S",
            r#"<conditionalFormatting sqref="A1:A5"><cfRule type="expression" priority="1"><formula>A1&gt;0</formula></cfRule></conditionalFormatting>"#,
        );
        delete_rows(&mut pkg.workbook, 0, 0, 1);
        let cf = &pkg.workbook.sheets[0].cond_formats[0];
        assert_eq!(cf.ranges, vec![(0, 0, 3, 0)]);
        // The new anchor is the old A2, which read A2: one row up now, A1.
        assert_eq!(first_formula(cf), "A1>0");
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(
            ws.contains(r#"<conditionalFormatting sqref="A1:A4"><cfRule type="expression" priority="1"><formula>A1&gt;0</formula>"#),
            "{ws}"
        );
    }

    #[test]
    fn cf_relative_ref_survives_anchor_column_delete() {
        let mut pkg = one(
            "S",
            r#"<conditionalFormatting sqref="B1:D1"><cfRule type="cellIs" operator="greaterThan" priority="1"><formula>B2</formula></cfRule></conditionalFormatting>"#,
        );
        delete_cols(&mut pkg.workbook, 0, 0, 2); // A:B
        let cf = &pkg.workbook.sheets[0].cond_formats[0];
        assert_eq!(cf.ranges, vec![(0, 0, 0, 1)]);
        // The old C1 read C2; it is A1 now and reads A2.
        assert_eq!(first_formula(cf), "A2");
    }

    #[test]
    fn dv_relative_ref_survives_anchor_row_delete() {
        let mut pkg = one(
            "S",
            r#"<dataValidations count="1"><dataValidation type="custom" sqref="A1:A5"><formula1>ISNUMBER(A1)</formula1></dataValidation></dataValidations>"#,
        );
        delete_rows(&mut pkg.workbook, 0, 0, 1);
        let dv = &pkg.workbook.sheets[0].validations[0];
        assert_eq!(dv.ranges, vec![(0, 0, 3, 0)]);
        assert_eq!(dv.formula1, "ISNUMBER(A1)");
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(
            ws.contains(
                r#"<dataValidation type="custom" sqref="A1:A4"><formula1>ISNUMBER(A1)</formula1>"#
            ),
            "{ws}"
        );
    }

    #[test]
    fn dv_list_on_other_sheet_follows_its_source_sheet() {
        let report = format!(
            r#"{DATA}<conditionalFormatting sqref="B1:B5"><cfRule type="expression" priority="1"><formula>COUNTIF(Lists!$A$1:$A$5,B1)&gt;0</formula></cfRule></conditionalFormatting><dataValidations count="1"><dataValidation type="list" sqref="A1:A5"><formula1>Lists!$A$1:$A$5</formula1></dataValidation></dataValidations>"#
        );
        let mut pkg = load_xlsx(&book(
            "",
            &[("Report", Some(&report)), ("Lists", Some(DATA))],
        ))
        .expect("loads");
        insert_rows(&mut pkg.workbook, 1, 0, 2);
        rename_sheet(&mut pkg.workbook, 1, "New Lists");

        let s = &pkg.workbook.sheets[0];
        assert_eq!(s.validations[0].ranges, vec![(0, 0, 4, 0)]);
        assert_eq!(s.validations[0].formula1, "'New Lists'!$A$3:$A$7");
        assert_eq!(s.cond_formats[0].ranges, vec![(0, 1, 4, 1)]);
        assert_eq!(
            first_formula(&s.cond_formats[0]),
            "COUNTIF('New Lists'!$A$3:$A$7,B1)>0"
        );
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(
            ws.contains(r#"<dataValidation type="list" sqref="A1:A5"><formula1>'New Lists'!$A$3:$A$7</formula1>"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<conditionalFormatting sqref="B1:B5"><cfRule type="expression" priority="1"><formula>COUNTIF('New Lists'!$A$3:$A$7,B1)&gt;0</formula>"#),
            "{ws}"
        );
    }

    #[test]
    fn untouched_cf_and_dv_keep_their_bytes() {
        let cf = r#"<conditionalFormatting sqref="A1:A5" extra="u"><cfRule type="cellIs" operator="between" dxfId="0" priority="1" stopIfTrue="1"><formula>_xlfn.XOR(A1,B1)</formula><formula>  A1 &lt;  5 </formula></cfRule></conditionalFormatting>"#;
        let dv = r#"<dataValidations count="1" disablePrompts="0"><dataValidation sqref="B1:B5" type="custom" showErrorMessage="1"><formula1>_xlfn.ISFORMULA(A1)</formula1></dataValidation></dataValidations>"#;
        let other = format!("{DATA}{cf}{dv}");
        let mut pkg = load_xlsx(&book(
            "",
            &[("Report", Some(&other)), ("Other", Some(&other))],
        ))
        .expect("loads");
        // Below and right of every range, and on the other sheet.
        insert_rows(&mut pkg.workbook, 0, 10, 2);
        delete_cols(&mut pkg.workbook, 0, 5, 1);
        insert_rows(&mut pkg.workbook, 1, 0, 3);
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(cf), "{ws}");
        assert!(ws.contains(dv), "{ws}");
        // The other sheet's moved: only it is rewritten.
        let ws2 = part(&saved(&pkg, SHEET2).0, SHEET2);
        assert!(ws2.contains(r#"sqref="A4:A8" extra="u""#), "{ws2}");
    }

    #[test]
    fn color_scale_cf_moves_its_sqref() {
        let mut pkg = one(
            "S",
            r#"<conditionalFormatting sqref="A1:A5"><cfRule type="colorScale" priority="1"><colorScale><cfvo type="min"/><cfvo type="max"/><color rgb="FFF8696B"/><color rgb="FF63BE7B"/></colorScale></cfRule><cfRule type="containsText" dxfId="0" priority="2" operator="containsText" text="x"><formula>NOT(ISERROR(SEARCH("x",A1)))</formula></cfRule></conditionalFormatting>"#,
        );
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let cf = &pkg.workbook.sheets[0].cond_formats[0];
        assert!(matches!(cf.rules[1].kind, CfKind::Other { .. }));
        assert_eq!(cf.rules[1].formulas()[0], "NOT(ISERROR(SEARCH(\"x\",A2)))");
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(
            ws.contains(r#"<conditionalFormatting sqref="A2:A6"><cfRule type="colorScale" priority="1"><colorScale><cfvo type="min"/><cfvo type="max"/><color rgb="FFF8696B"/><color rgb="FF63BE7B"/></colorScale></cfRule><cfRule type="containsText" dxfId="0" priority="2" operator="containsText" text="x"><formula>NOT(ISERROR(SEARCH("x",A2)))</formula></cfRule></conditionalFormatting>"#),
            "{ws}"
        );
    }

    #[test]
    fn added_cf_and_dv_move_on_insert() {
        // One of each from the file, one of each added.
        let mut pkg = one(
            "S",
            r#"<conditionalFormatting sqref="C1"><cfRule type="expression" priority="1"><formula>TRUE</formula></cfRule></conditionalFormatting><dataValidations count="1"><dataValidation type="whole" sqref="D1"><formula1>1</formula1></dataValidation></dataValidations>"#,
        );
        assert!(pkg.add_conditional_format(
            0,
            (0, 0, 1, 0),
            "greaterThan",
            "B1",
            None,
            Dxf::default()
        ));
        assert!(pkg.add_data_validation(0, (0, 1, 1, 1), "whole", "lessThan", "A1", None));
        let s = &pkg.workbook.sheets[0];
        assert_eq!(s.cond_formats[1].ix, Some(1));
        assert_eq!(s.validations[1].ix, Some(1));
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (re, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(r#"<conditionalFormatting sqref="C2">"#), "{ws}");
        assert!(
            ws.contains(r#"<conditionalFormatting sqref="A2:A3">"#),
            "{ws}"
        );
        assert!(ws.contains("<formula>B2</formula>"), "{ws}");
        assert!(
            ws.contains(r#"<dataValidation type="whole" sqref="D2">"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"sqref="B2:B3"><formula1>A2</formula1>"#),
            "{ws}"
        );
        assert_eq!(re.workbook.sheets[0].cond_formats.len(), 2);
        assert_eq!(re.workbook.sheets[0].validations.len(), 2);
    }

    #[test]
    fn added_to_a_fresh_sheet_moves_on_insert() {
        let mut pkg = new_xlsx();
        assert!(pkg.add_conditional_format(
            0,
            (0, 0, 1, 0),
            "greaterThan",
            "B1",
            None,
            Dxf::default()
        ));
        assert!(pkg.add_data_validation(0, (0, 1, 1, 1), "whole", "lessThan", "A1", None));
        insert_cols(&mut pkg.workbook, 0, 0, 1);
        let (re, _) = saved(&pkg, SHEET1);
        let s = &re.workbook.sheets[0];
        assert_eq!(s.cond_formats[0].ranges, vec![(0, 1, 1, 1)]);
        assert_eq!(first_formula(&s.cond_formats[0]), "C1");
        assert_eq!(s.validations[0].ranges, vec![(0, 2, 1, 2)]);
        assert_eq!(s.validations[0].formula1, "B1");
    }

    #[test]
    fn restored_model_writes_its_own_positions() {
        // AC8: the save compares the model with the part, so a model that an
        // undo or redo puts back writes ITS positions, whatever happened in
        // between.
        let body = r#"<conditionalFormatting sqref="A1:A5"><cfRule type="expression" priority="1"><formula>$B$5&gt;0</formula></cfRule></conditionalFormatting><conditionalFormatting sqref="B2"><cfRule type="expression" priority="2"><formula>TRUE</formula></cfRule></conditionalFormatting><dataValidations count="2"><dataValidation type="custom" sqref="C2"><formula1>C1</formula1></dataValidation><dataValidation type="custom" sqref="D1:D4"><formula1>$E$4</formula1></dataValidation></dataValidations>"#;
        let mut pkg = one("S", body);
        let original = pkg.workbook.clone();
        let read = |b: &[u8]| part(&load_xlsx(b).unwrap(), SHEET1);
        let before = read(&save_xlsx(&pkg));

        // Delete rows 1:2: B2 and C2 go, the rest trims, $B$5 / $E$4 move.
        delete_rows(&mut pkg.workbook, 0, 0, 2);
        let deleted = pkg.workbook.clone();
        // Then another edit, which an undo takes back to `deleted`.
        insert_rows(&mut pkg.workbook, 0, 0, 3);
        pkg.workbook = deleted;
        let (_, ws) = saved(&pkg, SHEET1);
        assert_eq!(count(&ws, "<conditionalFormatting"), 1, "{ws}");
        assert!(
            ws.contains(r#"<conditionalFormatting sqref="A1:A3"><cfRule type="expression" priority="1"><formula>$B$3&gt;0</formula>"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<dataValidations count="1"><dataValidation type="custom" sqref="D1:D2"><formula1>$E$2</formula1></dataValidation></dataValidations>"#),
            "{ws}"
        );

        // And all the way back: the loaded positions, byte for byte.
        pkg.workbook = original;
        assert_eq!(read(&save_xlsx(&pkg)), before);
    }

    #[test]
    fn a_raw_gt_in_an_attribute_neither_moves_nor_duplicates_sqref() {
        // `>` is legal unescaped in an attribute value; it must not end the
        // start tag before `sqref`.
        let dv = r#"<dataValidations count="1"><dataValidation type="whole" error="must be > 0" sqref="A1:A5"><formula1>1</formula1></dataValidation></dataValidations>"#;
        let cf = r#"<conditionalFormatting pivot="0" note="a>b" sqref="B1:B5"><cfRule type="expression" priority="1"><formula>B1&gt;0</formula></cfRule></conditionalFormatting>"#;
        let body = format!("{cf}{dv}");
        let pkg = one("S", &body);
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(cf), "{ws}");
        assert!(ws.contains(dv), "{ws}");

        let mut pkg = one("S", &body);
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (re, ws) = saved(&pkg, SHEET1);
        assert_eq!(count(&ws, "sqref="), 2, "{ws}");
        assert!(
            ws.contains(r#"<dataValidation type="whole" error="must be > 0" sqref="A2:A6"><formula1>1</formula1>"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<conditionalFormatting pivot="0" note="a>b" sqref="B2:B6"><cfRule type="expression" priority="1"><formula>B2&gt;0</formula>"#),
            "{ws}"
        );
        assert_eq!(
            re.workbook.sheets[0].validations[0].ranges,
            vec![(1, 0, 5, 0)]
        );
    }

    #[test]
    fn an_element_without_sqref_is_left_alone() {
        // The loader keeps a CF block with rules but no `sqref`: no ranges,
        // and an `ix`. The edit moves its formula in the model, but with no
        // `sqref` to read the save can't tell what the element covers, so it
        // leaves it as it is rather than rewrite it (or add an `sqref`).
        let cf = r#"<conditionalFormatting><cfRule type="expression" priority="1"><formula>A1</formula></cfRule></conditionalFormatting>"#;
        let mut pkg = one("S", cf);
        assert_eq!(pkg.workbook.sheets[0].cond_formats[0].ix, Some(0));
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        assert_eq!(first_formula(&pkg.workbook.sheets[0].cond_formats[0]), "A2");
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(cf), "{ws}");
        // A DV without one isn't modelled at all (no ranges), so it is
        // left as it is by construction; kept here as a regression guard.
        let dv = r#"<dataValidations count="1"><dataValidation type="whole"><formula1>A1</formula1></dataValidation></dataValidations>"#;
        let mut pkg = one("S", dv);
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(dv), "{ws}");
    }

    #[test]
    fn x14_cf_in_extlst_is_left_alone() {
        let x14 = r#"<extLst><ext uri="{78C0D931-6437-407d-A8EE-F0AAD7539E65}" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main"><x14:conditionalFormattings><x14:conditionalFormatting xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main"><x14:cfRule type="expression" priority="2" id="{X}"><xm:f>A1&gt;1</xm:f></x14:cfRule><xm:sqref>A1:A5</xm:sqref></x14:conditionalFormatting></x14:conditionalFormattings></ext></extLst>"#;
        let mut pkg = one(
            "S",
            &format!(
                r#"<conditionalFormatting sqref="B1:B5"><cfRule type="expression" priority="1"><formula>B1&gt;0</formula></cfRule></conditionalFormatting>{MARGINS}{x14}"#
            ),
        );
        let s = &pkg.workbook.sheets[0];
        assert_eq!(s.cond_formats.len(), 2);
        assert_eq!(s.cond_formats[0].ix, Some(0));
        assert_eq!(s.cond_formats[1].ix, None);
        // A delete over the x14 block's (unread) cells doesn't drop it.
        delete_rows(&mut pkg.workbook, 0, 0, 10);
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        assert_eq!(pkg.workbook.sheets[0].cond_formats.len(), 1);
        assert_eq!(pkg.workbook.sheets[0].cond_formats[0].ix, None);
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(x14), "{ws}");
        assert!(!ws.contains(r#"<conditionalFormatting sqref"#), "{ws}");
    }

    #[test]
    fn prefixed_part_with_comment_between_blocks_moves_the_right_element() {
        let mut pkg = new_xlsx();
        let first = r#"<x:conditionalFormatting sqref="A1"><x:cfRule type="expression" priority="1"><x:formula>TRUE</x:formula></x:cfRule></x:conditionalFormatting>"#;
        pkg.set_part(
            SHEET1,
            format!(
                r#"<?xml version="1.0"?><x:worksheet xmlns:x="{NS}"><x:sheetData/>{first}<!-- <x:conditionalFormatting sqref="Z9"/> --><x:conditionalFormatting sqref="C3"><x:cfRule type="expression" priority="2"><x:formula>C3=1</x:formula></x:cfRule></x:conditionalFormatting><x:dataValidations count="1"><!-- --><x:dataValidation type="custom" sqref="C3"><x:formula1>C3&gt;1</x:formula1></x:dataValidation></x:dataValidations></x:worksheet>"#
            )
            .into_bytes(),
        );
        let mut pkg = load_xlsx(&write_zip(&pkg.parts)).expect("load");
        let s = &pkg.workbook.sheets[0];
        assert_eq!(
            s.cond_formats.iter().map(|c| c.ix).collect::<Vec<_>>(),
            vec![Some(0), Some(1)]
        );
        assert_eq!(s.validations[0].ix, Some(0));
        insert_rows(&mut pkg.workbook, 0, 1, 1); // below A1, above C3
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(first), "{ws}");
        assert!(
            ws.contains(r#"<!-- <x:conditionalFormatting sqref="Z9"/> -->"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<x:conditionalFormatting sqref="C4"><x:cfRule type="expression" priority="2"><x:formula>C4=1</x:formula>"#),
            "{ws}"
        );
        assert!(
            ws.contains(
                r#"<x:dataValidation type="custom" sqref="C4"><x:formula1>C4&gt;1</x:formula1>"#
            ),
            "{ws}"
        );
    }

    #[test]
    fn a_stale_claim_from_a_restored_model_yields_to_an_added_block() {
        let mut pkg = one(
            "S",
            r#"<conditionalFormatting sqref="A1"><cfRule type="expression" priority="1"><formula>TRUE</formula></cfRule></conditionalFormatting>"#,
        );
        let snapshot = pkg.workbook.clone();
        assert!(pkg.clear_conditional_formats(0));
        // An undo that restores the model but not the part.
        pkg.workbook = snapshot;
        assert!(pkg.add_conditional_format(
            0,
            (2, 2, 2, 2),
            "greaterThan",
            "1",
            None,
            Dxf::default()
        ));
        let s = &pkg.workbook.sheets[0];
        assert_eq!(s.cond_formats[0].ix, None);
        assert_eq!(s.cond_formats[1].ix, Some(0));
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (_, ws) = saved(&pkg, SHEET1);
        assert_eq!(count(&ws, "<conditionalFormatting"), 1, "{ws}");
        assert!(ws.contains(r#"<conditionalFormatting sqref="C4">"#), "{ws}");
    }

    #[test]
    fn an_element_two_blocks_claim_is_left_alone() {
        let cf = r#"<conditionalFormatting sqref="A1"><cfRule type="expression" priority="1"><formula>A1</formula></cfRule></conditionalFormatting>"#;
        let mut pkg = one("S", cf);
        let mut twin = pkg.workbook.sheets[0].cond_formats[0].clone();
        twin.ranges = vec![(5, 5, 5, 5)];
        pkg.workbook.sheets[0].cond_formats.push(twin);
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(cf), "{ws}");
    }

    #[test]
    fn unparseable_sqref_token_element_left_alone() {
        let cf = r#"<conditionalFormatting sqref="A:A B1:B2"><cfRule type="expression" priority="1"><formula>TRUE</formula></cfRule></conditionalFormatting>"#;
        let mut pkg = one("S", cf);
        assert_eq!(
            pkg.workbook.sheets[0].cond_formats[0].ranges,
            vec![(0, 1, 1, 1)]
        );
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(cf), "{ws}");
        // Nor is it removed when the ranges it could read are all deleted.
        let mut pkg = one("S", cf);
        delete_rows(&mut pkg.workbook, 0, 0, 2);
        assert_eq!(pkg.workbook.sheets[0].cf_removed, vec![0]);
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(cf), "{ws}");
    }

    #[test]
    fn cf_formula_text_is_decoded_on_load() {
        let pkg = one(
            "S",
            r#"<conditionalFormatting sqref="A1"><cfRule type="expression" priority="1"><formula>A1&lt;5</formula></cfRule></conditionalFormatting>"#,
        );
        assert_eq!(
            first_formula(&pkg.workbook.sheets[0].cond_formats[0]),
            "A1<5"
        );
    }

    /// A CF block over `sqref` with one expression rule `formula`.
    fn expr_cf(sqref: &str, formula: &str) -> String {
        format!(
            r#"<conditionalFormatting sqref="{sqref}"><cfRule type="expression" priority="1"><formula>{formula}</formula></cfRule></conditionalFormatting>"#
        )
    }

    #[test]
    fn deleting_the_range_that_held_the_anchor_column_retranslates() {
        // Anchor (row 1, col A); A5:A6 held its column. Once it goes, C1
        // anchors, and it read C1.
        let mut pkg = one("S", &expr_cf("C1:C2 A5:A6", "A1&gt;0"));
        delete_rows(&mut pkg.workbook, 0, 4, 2);
        let cf = &pkg.workbook.sheets[0].cond_formats[0];
        assert_eq!(cf.ranges, vec![(0, 2, 1, 2)]);
        assert_eq!(first_formula(cf), "C1>0");
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(ws.contains(&expr_cf("C1:C2", "C1&gt;0")), "{ws}");
    }

    #[test]
    fn deleting_the_range_that_held_the_anchor_row_and_column_retranslates() {
        // A1:A2 goes; the old C5 (now C3) anchors, and it read C5.
        let mut pkg = one("S", &expr_cf("A1:A2 C5:C6", "A1&gt;0"));
        delete_rows(&mut pkg.workbook, 0, 0, 2);
        let cf = &pkg.workbook.sheets[0].cond_formats[0];
        assert_eq!(cf.ranges, vec![(2, 2, 3, 2)]);
        assert_eq!(first_formula(cf), "C3>0");
    }

    #[test]
    fn deleting_the_columns_that_held_the_anchor_row_retranslates() {
        // C1:D1 held the anchor's row; once those columns go, A2 anchors.
        let mut pkg = one("S", &expr_cf("A2:A3 C1:D1", "A1&gt;0"));
        delete_cols(&mut pkg.workbook, 0, 2, 2);
        let cf = &pkg.workbook.sheets[0].cond_formats[0];
        assert_eq!(cf.ranges, vec![(1, 0, 2, 0)]);
        assert_eq!(first_formula(cf), "A2>0");
    }

    #[test]
    fn dv_deleting_the_range_that_held_the_anchor_column_retranslates() {
        let mut pkg = one(
            "S",
            r#"<dataValidations count="1"><dataValidation type="custom" sqref="C1:C2 A5:A6"><formula1>ISNUMBER(A1)</formula1></dataValidation></dataValidations>"#,
        );
        delete_rows(&mut pkg.workbook, 0, 4, 2);
        let dv = &pkg.workbook.sheets[0].validations[0];
        assert_eq!(dv.ranges, vec![(0, 2, 1, 2)]);
        assert_eq!(dv.formula1, "ISNUMBER(C1)");
        let (_, ws) = saved(&pkg, SHEET1);
        assert!(
            ws.contains(r#"sqref="C1:C2"><formula1>ISNUMBER(C1)</formula1>"#),
            "{ws}"
        );
    }

    /// Add a CF over C3 to a part whose blocks stand where the new one lands
    /// ahead of some of them, move rows, and check each element kept its own
    /// rule.
    fn add_into_misordered(body: &str, added_ix: usize, existing: &[(&str, &str)]) {
        let mut pkg = one("S", body);
        assert!(pkg.add_conditional_format(
            0,
            (2, 2, 2, 2),
            "greaterThan",
            "5",
            None,
            Dxf::default()
        ));
        let s = &pkg.workbook.sheets[0];
        assert_eq!(s.cond_formats.last().unwrap().ix, Some(added_ix));
        insert_rows(&mut pkg.workbook, 0, 0, 1);
        let (re, ws) = saved(&pkg, SHEET1);
        for (sqref, formula) in existing {
            assert!(ws.contains(&expr_cf(sqref, formula)), "{sqref}: {ws}");
        }
        assert!(
            ws.contains(r#"<conditionalFormatting sqref="C4"><cfRule type="cellIs" dxfId="0" priority="2" operator="greaterThan"><formula>5</formula>"#),
            "{ws}"
        );
        assert_eq!(re.workbook.sheets[0].cond_formats.len(), existing.len() + 1);
    }

    #[test]
    fn a_cf_added_ahead_of_a_misplaced_block_keeps_both_in_place() {
        // A block after <dataValidations>, which ranks after it: the new
        // block goes before the DVs, so ahead of it.
        let body = format!(
            r#"<dataValidations count="1"><dataValidation type="whole" sqref="D1"><formula1>1</formula1></dataValidation></dataValidations>{}"#,
            expr_cf("A1", "A1=1")
        );
        add_into_misordered(&body, 0, &[("A2", "A2=1")]);
    }

    #[test]
    fn a_cf_added_between_blocks_split_by_margins_keeps_each_in_place() {
        let body = format!(
            "{}{MARGINS}{}",
            expr_cf("A1", "A1=1"),
            expr_cf("B1", "B1=2")
        );
        add_into_misordered(&body, 1, &[("A2", "A2=1"), ("B2", "B2=2")]);
    }
}

#[cfg(test)]
mod table_command_tests {
    use super::*;
    use crate::edit::{convert_table_to_range, rename_table, resize_table};
    use crate::sheet::{Cell, DefinedName};

    fn text(pkg: &SheetPackage, part: &str) -> String {
        String::from_utf8(pkg.part(part).unwrap_or_default().to_vec()).unwrap()
    }

    fn reload(pkg: &SheetPackage) -> SheetPackage {
        load_xlsx(&save_xlsx(pkg)).unwrap()
    }

    /// Sheet1: Item/Qty over A1:B3 as `Table1`, `=SUM(Table1[Qty])` in D1.
    fn one_table() -> SheetPackage {
        let mut pkg = new_xlsx();
        let s = &mut pkg.workbook.sheets[0];
        s.set_cell(0, 0, Cell::text("Item"));
        s.set_cell(0, 1, Cell::text("Qty"));
        s.set_cell(1, 0, Cell::text("Pen"));
        s.set_cell(1, 1, Cell::number(3.0));
        s.set_cell(2, 0, Cell::text("Ink"));
        s.set_cell(2, 1, Cell::number(4.0));
        s.set_cell(0, 3, Cell::formula("SUM(Table1[Qty])"));
        pkg.add_table(0, (0, 0, 2, 1), true, "TableStyleMedium2")
            .unwrap();
        pkg
    }

    /// Give the `name` column of the table in `part` a calculated-column
    /// formula, as Excel writes them.
    fn calculated(pkg: &mut SheetPackage, part: &str, name: &str, formula: &str) {
        let xml = text(pkg, part);
        let at = xml.find(&format!("name=\"{name}\"/>")).unwrap();
        let end = at + format!("name=\"{name}\"/>").len();
        let el = format!(
            "name=\"{name}\"><calculatedColumnFormula>{}</calculatedColumnFormula></tableColumn>",
            esc_text(formula)
        );
        let xml = format!("{}{el}{}", &xml[..at], &xml[end..]);
        pkg.parts.iter_mut().find(|(n, _)| n == part).unwrap().1 = xml.into_bytes();
    }

    fn column_formula(pkg: &SheetPackage, part: &str) -> String {
        let xml = text(pkg, part);
        let s = xml.find("<calculatedColumnFormula>").unwrap() + "<calculatedColumnFormula>".len();
        let e = xml.find("</calculatedColumnFormula>").unwrap();
        decode(&xml[s..e])
    }

    #[test]
    fn a_renamed_table_saves_under_its_new_name() {
        let mut pkg = one_table();
        pkg.workbook.defined_names.push(DefinedName {
            name: "Everything".into(),
            scope: None,
            formula: "Table1[#All]".into(),
        });
        rename_table(&mut pkg.workbook, "Table1", "Revenue").unwrap();
        let re = reload(&pkg);
        assert_eq!(re.workbook.tables[0].name, "Revenue");
        let part = text(&re, &re.workbook.tables[0].part);
        assert!(part.contains("name=\"Revenue\""), "{part}");
        assert!(part.contains("displayName=\"Revenue\""), "{part}");
        assert!(!part.contains("Table1"), "{part}");
        let d1 = re.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.formula.as_deref(), Some("SUM(Revenue[Qty])"));
        let value = crate::engine::eval_formula_at(&re.workbook, 0, 0, 3, "SUM(Revenue[Qty])");
        assert_eq!(value, crate::formula::Value::Num(7.0));
        assert_eq!(re.workbook.defined_names[0].formula, "Revenue[#All]");
        // A second save of the same package says the same thing.
        let again = reload(&re);
        assert_eq!(again.workbook.tables[0].name, "Revenue");
    }

    #[test]
    fn renames_reach_the_column_formulas_of_every_table_part() {
        let mut pkg = one_table();
        let s = &mut pkg.workbook.sheets[0];
        s.set_cell(5, 0, Cell::text("Key"));
        s.set_cell(5, 1, Cell::text("Calc"));
        s.set_cell(6, 0, Cell::text("x"));
        pkg.add_table(0, (5, 0, 6, 1), true, "TableStyleMedium2")
            .unwrap();
        let part2 = pkg.workbook.tables[1].part.clone();
        calculated(&mut pkg, &part2, "Calc", "SUM(Table1[Qty])+ROWS(Table2)");
        // A -> C, then B -> A: one substitution, not two in a row.
        rename_table(&mut pkg.workbook, "Table1", "Sales").unwrap();
        rename_table(&mut pkg.workbook, "Table2", "Table1").unwrap();
        let re = reload(&pkg);
        assert_eq!(column_formula(&re, &part2), "SUM(Sales[Qty])+ROWS(Table1)");
        let names: Vec<&str> = re.workbook.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["Sales", "Table1"]);
    }

    #[test]
    fn rewritten_column_formulas_keep_the_file_spelling() {
        let mut pkg = one_table();
        let s = &mut pkg.workbook.sheets[0];
        s.set_cell(5, 0, Cell::text("Key"));
        s.set_cell(5, 1, Cell::text("Calc"));
        s.set_cell(6, 0, Cell::text("x"));
        pkg.add_table(0, (5, 0, 6, 1), true, "TableStyleMedium2")
            .unwrap();
        let part2 = pkg.workbook.tables[1].part.clone();
        calculated(
            &mut pkg,
            &part2,
            "Key",
            "_xlfn.XLOOKUP(1,Table1[Qty],Table1[Item])",
        );
        calculated(&mut pkg, &part2, "Calc", "Table1[[#This Row],[Qty]]*2");
        rename_table(&mut pkg.workbook, "Table1", "Sales").unwrap();
        let xml = text(&reload(&pkg), &part2);
        assert!(
            xml.contains("_xlfn.XLOOKUP(1,Sales[Qty],Sales[Item])"),
            "{xml}"
        );
        assert!(xml.contains("Sales[[#This Row],[Qty]]*2"), "{xml}");
    }

    #[test]
    fn a_new_table_name_is_unique_ignoring_case() {
        let mut pkg = one_table();
        rename_table(&mut pkg.workbook, "Table1", "table2").unwrap();
        pkg.workbook.defined_names.push(DefinedName {
            name: "TABLE3".into(),
            scope: None,
            formula: "1".into(),
        });
        pkg.workbook.sheets[0].set_cell(9, 0, Cell::text("H"));
        let i = pkg
            .add_table(0, (9, 0, 10, 0), true, "TableStyleMedium2")
            .unwrap();
        assert_eq!(pkg.workbook.tables[i].name, "Table4");
    }

    #[test]
    fn a_converted_table_leaves_the_file() {
        let mut pkg = one_table();
        let s = &mut pkg.workbook.sheets[0];
        s.set_cell(5, 0, Cell::text("Key"));
        s.set_cell(5, 1, Cell::text("Calc"));
        s.set_cell(6, 0, Cell::text("x"));
        pkg.add_table(0, (5, 0, 6, 1), true, "TableStyleMedium2")
            .unwrap();
        let part2 = pkg.workbook.tables[1].part.clone();
        calculated(&mut pkg, &part2, "Calc", "SUM(Table1[Qty])");
        let part1 = pkg.workbook.tables[0].part.clone();
        convert_table_to_range(&mut pkg.workbook, "Table1").unwrap();
        let re = reload(&pkg);
        assert_eq!(re.workbook.tables.len(), 1);
        assert!(re.part(&part1).is_none(), "the table part is dropped");
        let d1 = re.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.formula.as_deref(), Some("SUM($B$2:$B$3)"));
        assert_eq!(column_formula(&re, &part2), "SUM($B$2:$B$3)");
        let ws = text(&re, &re.sheet_parts[0].clone());
        assert_eq!(ws.matches("<tablePart ").count(), 1, "{ws}");
        assert!(ws.contains("<tableParts count=\"1\">"), "{ws}");
        let rels = text(&re, "xl/worksheets/_rels/sheet1.xml.rels");
        let file = part1.rsplit('/').next().unwrap();
        assert!(!rels.contains(file), "{rels}");
        let ct = text(&re, "[Content_Types].xml");
        assert!(!ct.contains(&format!("/{part1}")), "{ct}");
    }

    /// Table1 (A1:B3, or A1:B4 with a totals row) converted to a range after
    /// Table2 (A11:B12) took the column formula `formula` over it, and a cell
    /// far off (Z100) held the same formula.
    fn converted_under_a_column_formula(formula: &str, totals: bool) -> (SheetPackage, String) {
        let mut pkg = one_table();
        let s = &mut pkg.workbook.sheets[0];
        s.set_cell(10, 0, Cell::text("Key"));
        s.set_cell(10, 1, Cell::text("Calc"));
        s.set_cell(11, 0, Cell::text("x"));
        s.set_cell(99, 25, Cell::formula(formula));
        if totals {
            s.set_cell(3, 0, Cell::text("Total"));
            s.set_cell(3, 1, Cell::formula("SUBTOTAL(109,[Qty])"));
            pkg.workbook.tables[0].range = (0, 0, 3, 1);
            pkg.workbook.tables[0].totals_rows = 1;
        }
        pkg.add_table(0, (10, 0, 11, 1), true, "TableStyleMedium2")
            .unwrap();
        let part2 = pkg.workbook.tables[1].part.clone();
        calculated(&mut pkg, &part2, "Calc", formula);
        convert_table_to_range(&mut pkg.workbook, "Table1").unwrap();
        (pkg, part2)
    }

    /// The formula of the one cell in column Z (the parity cell, wherever
    /// the edits moved it).
    fn parity_cell(pkg: &SheetPackage) -> String {
        let cells = &pkg.workbook.sheets[0].cells;
        let mut z = cells.iter().filter(|((_, c), _)| *c >= 20);
        let (_, cell) = z.next().expect("the parity cell");
        assert!(z.next().is_none());
        cell.formula.clone().unwrap()
    }

    #[test]
    fn a_converted_tables_column_formulas_read_what_its_cell_formulas_read() {
        use crate::edit::{delete_cols, delete_rows, insert_cols, insert_rows};
        type Edit = fn(&mut Workbook);
        let cases: Vec<(&str, bool, Vec<Edit>)> = vec![
            // A row above, two columns to the left.
            (
                "SUM(Table1[Qty])",
                false,
                vec![|wb| insert_rows(wb, 0, 0, 1), |wb| insert_cols(wb, 0, 0, 2)],
            ),
            // A column inside widens it.
            (
                "SUM(Table1[[Item]:[Qty]])",
                false,
                vec![|wb| insert_cols(wb, 0, 1, 1)],
            ),
            // Deleting the first column of a span keeps the rest (r3 M1).
            (
                "SUM(Table1[[Item]:[Qty]])&COUNTA(Table1[Item])",
                false,
                vec![|wb| delete_cols(wb, 0, 0, 1)],
            ),
            // A row inserted at the first data row moves the data (r3 M2).
            (
                "SUM(Table1[Qty])+ROWS(Table1[#All])",
                false,
                vec![|wb| insert_rows(wb, 0, 1, 1)],
            ),
            // …and at the totals row.
            (
                "SUM(Table1[Qty])+SUM(Table1[#Totals])",
                true,
                vec![|wb| insert_rows(wb, 0, 3, 2)],
            ),
            // Deleting the header row, then a data row. (A `#REF!` range
            // such as `#REF!:#REF!` stops a cell formula following later
            // edits, since its text no longer parses, so none is made here.)
            (
                "SUM(Table1[Qty])+ROWS(Table1[#All])",
                false,
                vec![|wb| delete_rows(wb, 0, 0, 1), |wb| delete_rows(wb, 0, 1, 1)],
            ),
            // All of its rows go.
            (
                "SUM(Table1[Qty])",
                false,
                vec![|wb| delete_rows(wb, 0, 0, 3)],
            ),
        ];
        for (formula, totals, edits) in cases {
            let (mut pkg, part2) = converted_under_a_column_formula(formula, totals);
            for edit in &edits {
                edit(&mut pkg.workbook);
            }
            let re = reload(&pkg);
            assert_eq!(
                column_formula(&re, &part2),
                parity_cell(&re),
                "{formula} after {} edits",
                edits.len()
            );
        }
    }

    #[test]
    fn a_converted_tables_column_formulas_follow_the_edits() {
        // Pins the parity test to real cell references.
        let (mut pkg, part2) = converted_under_a_column_formula("SUM(Table1[Qty])", false);
        crate::edit::insert_rows(&mut pkg.workbook, 0, 1, 1);
        assert_eq!(column_formula(&reload(&pkg), &part2), "SUM($B$3:$B$4)");
    }

    #[test]
    fn a_column_formula_on_another_sheet_follows_only_the_tables_sheet() {
        let mut pkg = one_table();
        let s2 = pkg.add_sheet("Other");
        let s = &mut pkg.workbook.sheets[s2];
        s.set_cell(0, 0, Cell::text("Key"));
        s.set_cell(0, 1, Cell::text("Calc"));
        s.set_cell(1, 0, Cell::text("x"));
        pkg.add_table(s2, (0, 0, 1, 1), true, "TableStyleMedium2")
            .unwrap();
        let part2 = pkg.workbook.tables[1].part.clone();
        calculated(&mut pkg, &part2, "Calc", "SUM(Table1[Qty])");
        convert_table_to_range(&mut pkg.workbook, "Table1").unwrap();
        crate::edit::insert_rows(&mut pkg.workbook, 0, 0, 1);
        crate::edit::insert_rows(&mut pkg.workbook, s2, 0, 5);
        assert_eq!(
            column_formula(&reload(&pkg), &part2),
            "SUM(Sheet1!$B$3:$B$4)"
        );
    }

    #[test]
    fn converting_the_only_table_removes_table_parts() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        convert_table_to_range(&mut pkg.workbook, "Table1").unwrap();
        let re = reload(&pkg);
        assert!(re.workbook.tables.is_empty());
        assert!(re.part(&part).is_none());
        assert!(!re.parts.iter().any(|(n, _)| n.starts_with("xl/tables/")));
        let ws = text(&re, &re.sheet_parts[0].clone());
        assert!(!ws.contains("tablePart"), "{ws}");
        let rels = text(&re, "xl/worksheets/_rels/sheet1.xml.rels");
        assert!(!rels.contains("/table\""), "{rels}");
    }

    #[test]
    fn an_undone_conversion_saves_the_table() {
        let mut pkg = one_table();
        let (tables, removed) = (
            pkg.workbook.tables.clone(),
            pkg.workbook.removed_tables.clone(),
        );
        let cells = pkg.workbook.sheets.clone();
        convert_table_to_range(&mut pkg.workbook, "Table1").unwrap();
        // Undo restores the snapshot: tables, removed tables, cells.
        pkg.workbook.tables = tables;
        pkg.workbook.removed_tables = removed;
        pkg.workbook.sheets = cells;
        let re = reload(&pkg);
        assert_eq!(re.workbook.tables.len(), 1);
        let d1 = re.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.formula.as_deref(), Some("SUM(Table1[Qty])"));
    }

    #[test]
    fn a_table_part_the_loader_skips_is_never_dropped() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        let xml = text(&pkg, &part).replace("ref=\"A1:B3\"", "ref=\"bogus\"");
        pkg.parts.iter_mut().find(|(n, _)| *n == part).unwrap().1 = xml.clone().into_bytes();
        // As a load would have it: the part is there, the table isn't.
        pkg.workbook.tables.clear();
        let loaded = reload(&pkg);
        assert!(loaded.workbook.tables.is_empty(), "the bad ref isn't read");
        let re = reload(&loaded);
        assert_eq!(text(&re, &part), xml);
        let ws = text(&re, &re.sheet_parts[0].clone());
        assert!(ws.contains("<tablePart "), "{ws}");
    }

    #[test]
    fn a_resized_table_saves_its_columns() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        resize_table(&mut pkg.workbook, "Table1", (0, 0, 4, 2)).unwrap();
        let re = reload(&pkg);
        let t = &re.workbook.tables[0];
        assert_eq!(t.range, (0, 0, 4, 2));
        assert_eq!(t.columns, vec!["Item", "Qty", "Column3"]);
        let xml = text(&re, &part);
        assert!(xml.contains("<tableColumns count=\"3\">"), "{xml}");
        assert!(
            xml.contains("<tableColumn id=\"3\" name=\"Column3\"/>"),
            "{xml}"
        );
        assert!(xml.contains("<autoFilter ref=\"A1:C5\""), "{xml}");
        let c1 = re.workbook.sheets[0].cell(0, 2).map(|c| c.value.clone());
        assert_eq!(c1, Some(CellValue::Text("Column3".into())));
        // Shrink to one column: Item goes, Qty keeps its id.
        let mut pkg = re;
        resize_table(&mut pkg.workbook, "Table1", (0, 1, 4, 1)).unwrap();
        let xml = text(&reload(&pkg), &part);
        assert!(
            xml.contains(
                "<tableColumns count=\"1\"><tableColumn id=\"2\" name=\"Qty\"/></tableColumns>"
            ),
            "{xml}"
        );
    }

    /// Type `text` into header cell (r, c) of sheet 0, as a host does.
    fn type_header(pkg: &mut SheetPackage, r: u32, c: u32, text: &str) -> bool {
        pkg.workbook.sheets[0].set_cell(r, c, Cell::text(text));
        crate::edit::sync_table_headers(&mut pkg.workbook, 0, &[(r, c)])
    }

    #[test]
    fn a_renamed_header_keeps_its_table_column_element() {
        let mut pkg = one_table();
        let s = &mut pkg.workbook.sheets[0];
        s.set_cell(5, 0, Cell::text("Key"));
        s.set_cell(5, 1, Cell::text("Calc"));
        s.set_cell(6, 0, Cell::text("x"));
        pkg.add_table(0, (5, 0, 6, 1), true, "TableStyleMedium2")
            .unwrap();
        let (part, part2) = (
            pkg.workbook.tables[0].part.clone(),
            pkg.workbook.tables[1].part.clone(),
        );
        calculated(&mut pkg, &part, "Item", "[@Qty]*2");
        calculated(&mut pkg, &part2, "Calc", "SUM(Table1[Qty])+COUNT([Calc])");
        let xml = text(&pkg, &part).replace(
            "<autoFilter ref=\"A1:B3\"/>",
            "<autoFilter ref=\"A1:B3\"><filterColumn colId=\"1\"><filters><filter val=\"3\"/></filters></filterColumn></autoFilter>",
        );
        assert!(xml.contains("colId=\"1\""), "{xml}");
        pkg.parts.iter_mut().find(|(n, _)| *n == part).unwrap().1 = xml.into_bytes();
        let mut pkg = reload(&pkg);
        assert!(type_header(&mut pkg, 0, 1, "Units"));
        // The table's own rename goes with it: the parts know the old names.
        rename_table(&mut pkg.workbook, "Table1", "Sales").unwrap();
        let re = reload(&pkg);
        assert_eq!(re.workbook.tables[0].columns, ["Item", "Units"]);
        let xml = text(&re, &part);
        assert!(xml.contains("<tableColumns count=\"2\">"), "{xml}");
        assert!(xml.contains("id=\"2\""), "{xml}");
        assert!(xml.contains("name=\"Units\""), "{xml}");
        assert!(!xml.contains("id=\"3\""), "no new element: {xml}");
        assert!(!xml.contains("Qty"), "{xml}");
        assert!(xml.contains("<filterColumn colId=\"1\">"), "{xml}");
        // The file's spelling of `[@Units]`.
        assert_eq!(column_formula(&re, &part), "[[#This Row],[Units]]*2");
        // Another table's unqualified `[Calc]` is its own column.
        assert_eq!(
            column_formula(&re, &part2),
            "SUM(Sales[Units])+COUNT([Calc])"
        );
        let d1 = re.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.formula.as_deref(), Some("SUM(Sales[Units])"));
        // Saved again, nothing more changes.
        assert_eq!(text(&reload(&re), &part), xml);
    }

    #[test]
    fn a_renamed_then_converted_tables_column_formulas_read_its_cells() {
        let mut pkg = one_table();
        let s = &mut pkg.workbook.sheets[0];
        s.set_cell(5, 0, Cell::text("Key"));
        s.set_cell(5, 1, Cell::text("Calc"));
        s.set_cell(6, 0, Cell::text("x"));
        pkg.add_table(0, (5, 0, 6, 1), true, "TableStyleMedium2")
            .unwrap();
        let part2 = pkg.workbook.tables[1].part.clone();
        calculated(&mut pkg, &part2, "Calc", "SUM(Table1[Qty])");
        let mut pkg = reload(&pkg);
        assert!(type_header(&mut pkg, 0, 1, "Units"));
        convert_table_to_range(&mut pkg.workbook, "Table1").unwrap();
        let d1 = pkg.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.formula.as_deref(), Some("SUM($B$2:$B$3)"));
        // The part still says `Qty`: it reads the cells D1 reads.
        assert_eq!(column_formula(&reload(&pkg), &part2), "SUM($B$2:$B$3)");
    }

    #[test]
    fn a_header_renamed_to_a_name_that_needs_brackets_reads_back() {
        for (name, d1) in [
            ("Price, USD", "SUM(Table1[[Price, USD]])"),
            ("Qty:kg", "SUM(Table1[[Qty:kg]])"),
            ("Qty ", "SUM(Table1[[Qty ]])"),
            ("Price (USD)", "SUM(Table1[[Price (USD)]])"),
            ("Total $ Amount", "SUM(Table1[[Total $ Amount]])"),
        ] {
            let mut pkg = reload(&one_table());
            assert!(type_header(&mut pkg, 0, 1, name));
            assert_eq!(pkg.workbook.tables[0].columns, ["Item", name]);
            let re = reload(&pkg);
            assert_eq!(re.workbook.tables[0].columns, ["Item", name]);
            let cell = re.workbook.sheets[0].cell(0, 3).unwrap();
            assert_eq!(cell.formula.as_deref(), Some(d1));
            let value = crate::engine::eval_formula_at(&re.workbook, 0, 0, 3, d1);
            assert_eq!(value, crate::formula::Value::Num(7.0), "{name:?}");
        }
    }

    #[test]
    fn a_deleted_column_leaves_its_query_table_field() {
        let mut pkg = one_table();
        resize_table(&mut pkg.workbook, "Table1", (0, 0, 2, 2)).unwrap();
        let part = pkg.workbook.tables[0].part.clone();
        let xml = text(&pkg, &part).replacen(" ref=", " tableType=\"queryTable\" ref=", 1);
        pkg.parts.iter_mut().find(|(n, _)| *n == part).unwrap().1 = xml.into_bytes();
        let (dir, file) = part.rsplit_once('/').unwrap();
        pkg.parts.push((
            format!("{dir}/_rels/{file}.rels"),
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/queryTable" Target="../queryTables/queryTable1.xml"/></Relationships>"#
                .to_vec(),
        ));
        let field = |id: u32, name: &str| {
            format!(r#"<queryTableField id="{id}" name="{name}" tableColumnId="{id}"/>"#)
        };
        let query = |fields: &str, count: usize| {
            format!(
                r#"<queryTable xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="Q" connectionId="1" autoFormatId="16" applyNumberFormats="0"><queryTableRefresh nextId="4"><queryTableFields count="{count}">{fields}</queryTableFields></queryTableRefresh></queryTable>"#
            )
        };
        let all = format!(
            "{}{}{}",
            field(1, "Item"),
            field(2, "Qty"),
            field(3, "Column3")
        );
        pkg.parts.push((
            "xl/queryTables/queryTable1.xml".into(),
            query(&all, 3).into_bytes(),
        ));
        let mut pkg = reload(&pkg);
        // Saved untouched while no column goes.
        assert_eq!(
            text(&reload(&pkg), "xl/queryTables/queryTable1.xml"),
            query(&all, 3)
        );
        crate::edit::delete_cols(&mut pkg.workbook, 0, 1, 1);
        let re = reload(&pkg);
        let kept = format!("{}{}", field(1, "Item"), field(3, "Column3"));
        assert_eq!(text(&re, "xl/queryTables/queryTable1.xml"), query(&kept, 2));
        assert_eq!(re.workbook.tables[0].column_ids, [1, 3]);
    }

    #[test]
    fn a_new_column_named_like_a_deleted_one_gets_a_new_element() {
        let mut pkg = one_table();
        resize_table(&mut pkg.workbook, "Table1", (0, 0, 2, 2)).unwrap();
        let mut pkg = reload(&pkg);
        let part = pkg.workbook.tables[0].part.clone();
        calculated(&mut pkg, &part, "Qty", "[@Item]");
        let mut pkg = reload(&pkg);
        assert_eq!(pkg.workbook.tables[0].column_ids, [1, 2, 3]);
        crate::edit::delete_cols(&mut pkg.workbook, 0, 1, 1);
        assert_eq!(pkg.workbook.tables[0].columns, ["Item", "Column3"]);
        pkg.workbook.sheets[0].set_cell(0, 2, Cell::text("Qty"));
        resize_table(&mut pkg.workbook, "Table1", (0, 0, 2, 2)).unwrap();
        assert_eq!(pkg.workbook.tables[0].columns, ["Item", "Column3", "Qty"]);
        let xml = text(&reload(&pkg), &part);
        assert!(
            xml.contains("<tableColumn id=\"4\" name=\"Qty\"/>"),
            "{xml}"
        );
        assert!(!xml.contains("calculatedColumnFormula"), "{xml}");
        assert!(!xml.contains("id=\"2\""), "{xml}");
    }

    #[test]
    fn a_table_whose_columns_were_all_deleted_leaves_the_package() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        crate::edit::delete_cols(&mut pkg.workbook, 0, 0, 2);
        assert!(pkg.workbook.tables.is_empty());
        let re = reload(&pkg);
        assert!(re.workbook.tables.is_empty());
        assert!(re.part(&part).is_none());
        let ws = text(&re, &re.sheet_parts[0].clone());
        assert!(!ws.contains("tablePart"), "{ws}");
        let rels = text(&re, "xl/worksheets/_rels/sheet1.xml.rels");
        assert!(!rels.contains("/table\""), "{rels}");
        let types = text(&re, "[Content_Types].xml");
        assert!(!types.contains(&part), "{types}");
        // D1 moved to B1; its reference to the table went with it.
        let b1 = re.workbook.sheets[0].cell(0, 1).unwrap();
        assert_eq!(b1.formula.as_deref(), Some("SUM(#REF!)"));
    }

    #[test]
    fn a_resize_moves_filters_with_their_columns() {
        let mut pkg = one_table();
        resize_table(&mut pkg.workbook, "Table1", (0, 0, 2, 2)).unwrap();
        let mut pkg = reload(&pkg);
        let part = pkg.workbook.tables[0].part.clone();
        let xml = text(&pkg, &part).replace(
            "<autoFilter ref=\"A1:C3\"/>",
            "<autoFilter ref=\"A1:C3\"><filterColumn colId=\"0\"><filters><filter val=\"Pen\"/></filters></filterColumn><filterColumn colId=\"2\"><filters><filter val=\"x\"/></filters></filterColumn></autoFilter><sortState ref=\"A2:C3\"><sortCondition ref=\"C2:C3\"/></sortState>",
        );
        assert!(xml.contains("colId=\"2\""), "{xml}");
        pkg.parts.iter_mut().find(|(n, _)| *n == part).unwrap().1 = xml.into_bytes();
        resize_table(&mut pkg.workbook, "Table1", (0, 1, 2, 2)).unwrap();
        let xml = text(&reload(&pkg), &part);
        assert!(
            xml.contains("<filterColumn colId=\"1\"><filters><filter val=\"x\"/>"),
            "{xml}"
        );
        assert!(
            !xml.contains("Pen"),
            "the dropped column's filter goes: {xml}"
        );
        assert!(!xml.contains("sortState"), "{xml}");
    }

    #[test]
    fn a_renamed_source_table_reaches_the_pivot_cache() {
        let mut pkg = one_table();
        let frame = crate::frame::Frame::from_range(&pkg.workbook, 0, (0, 0, 2, 1));
        let measure = crate::pivot::DataField {
            name: "Sum of Qty".into(),
            field: 1,
            agg: crate::frame::Agg::Sum,
        };
        let dest = pkg.add_sheet("Pivot");
        pkg.add_pivot(
            crate::pivot::PivotSource::Table("Table1".into()),
            frame.names.clone(),
            measure,
            dest,
            (2, 0),
        )
        .unwrap();
        let re = reload(&pkg);
        let mut pkg = re;
        rename_table(&mut pkg.workbook, "Table1", "Sales").unwrap();
        let re = reload(&pkg);
        assert_eq!(
            re.workbook.pivots[0].source,
            crate::pivot::PivotSource::Table("Sales".into())
        );
        let err = convert_table_to_range(&mut pkg.workbook, "Sales").unwrap_err();
        assert!(err.contains("uses this table"), "{err}");
    }

    #[test]
    fn add_table_refuses_what_excel_refuses() {
        let mut pkg = one_table();
        let before = pkg.parts.clone();
        let err = pkg
            .add_table(0, (1, 1, 4, 3), true, "TableStyleMedium2")
            .unwrap_err();
        assert_eq!(err, "The range overlaps table Table1");
        // A two-cell CSE array at F1:F2.
        let mut arr = Cell::formula("B2:B3*2");
        arr.f_attrs = Some("t=\"array\" ref=\"F1:F2\"".into());
        arr.spill = Some((2, 1));
        pkg.workbook.sheets[0].set_cell(0, 5, arr);
        let err = pkg
            .add_table(0, (1, 4, 3, 6), true, "TableStyleMedium2")
            .unwrap_err();
        assert!(err.contains("array formula"), "{err}");
        assert_eq!(pkg.parts, before, "a refusal writes nothing");
        assert_eq!(pkg.workbook.tables.len(), 1);
    }

    #[test]
    fn add_table_refuses_a_pivot_table_location() {
        let mut pkg = one_table();
        let frame = crate::frame::Frame::from_range(&pkg.workbook, 0, (0, 0, 2, 1));
        let measure = crate::pivot::DataField {
            name: "Sum of Qty".into(),
            field: 1,
            agg: crate::frame::Agg::Sum,
        };
        let dest = pkg.add_sheet("Pivot");
        let idx = pkg
            .add_pivot(
                crate::pivot::PivotSource::Range {
                    sheet: "Sheet1".into(),
                    rect: (0, 0, 2, 1),
                },
                frame.names.clone(),
                measure,
                dest,
                (2, 0),
            )
            .unwrap();
        let (r1, c1, _, _) = pkg.workbook.pivots[idx].location;
        let err = pkg
            .add_table(dest, (r1, c1, r1 + 3, c1 + 1), true, "TableStyleMedium2")
            .unwrap_err();
        assert!(err.contains("PivotTable"), "{err}");
    }

    /// #682: a part's `calculatedColumnFormula` loads into the model, and a
    /// save of an unchanged table writes its part as it was.
    #[test]
    fn calculated_formulas_load_and_resave_unchanged() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        calculated(&mut pkg, &part, "Qty", "LEN(Table1[[#This Row],[Item]])");
        let re = reload(&pkg);
        assert_eq!(
            re.workbook.tables[0].calculated_formulas,
            vec![None, Some("LEN(Table1[[#This Row],[Item]])".to_string())]
        );
        let saved = text(&re, &part);
        assert_eq!(text(&reload(&re), &part), saved);
        assert_eq!(saved, text(&pkg, &part));
    }

    /// A column renamed by its header renames it in the model's formula and
    /// in the part's alike, so the save keeps the part's rewrite.
    #[test]
    fn a_header_rename_reaches_the_model_formula_too() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        calculated(&mut pkg, &part, "Qty", "LEN(Table1[[#This Row],[Item]])");
        let mut re = reload(&pkg);
        re.workbook.sheets[0].set_cell(0, 0, Cell::text("Name"));
        crate::edit::sync_table_headers(&mut re.workbook, 0, &[(0, 0)]);
        let f = re.workbook.tables[0].calculated_formulas[1]
            .as_deref()
            .unwrap()
            .to_string();
        assert!(f.contains("Name") && !f.contains("Item"), "{f}");
        let again = reload(&re);
        assert_eq!(
            column_formula(&again, &part),
            "LEN(Table1[[#This Row],[Name]])"
        );
        let model = again.workbook.tables[0].calculated_formulas[1]
            .as_deref()
            .unwrap();
        assert_eq!(
            crate::formula::parse(model),
            crate::formula::parse("LEN(Table1[[#This Row],[Name]])")
        );
    }

    /// A grown table with a calculated formula on its new column saves the
    /// new `ref`, the column, and the formula in the file's spelling.
    #[test]
    fn a_new_calculated_formula_saves_on_a_grown_table() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        resize_table(&mut pkg.workbook, "Table1", (0, 0, 3, 2)).unwrap();
        pkg.workbook.tables[0].set_calculated_formula(2, Some("[@Qty]*2".into()));
        let re = reload(&pkg);
        let xml = text(&re, &part);
        assert!(xml.contains(r#"ref="A1:C4""#), "{xml}");
        assert!(
            xml.contains(r#"name="Column3"><calculatedColumnFormula>Table1[[#This Row],[Qty]]*2</calculatedColumnFormula></tableColumn>"#),
            "{xml}"
        );
        let t = &re.workbook.tables[0];
        assert_eq!(t.calculated_formulas.len(), 3);
        assert_eq!(
            t.calculated_formulas[2].as_deref(),
            Some("Table1[[#This Row],[Qty]]*2")
        );
        // Saved again, it says the same.
        assert_eq!(text(&reload(&re), &part), xml);
    }

    /// Put `el` in the part as column `name`'s only child, in place of its
    /// empty element.
    fn column_child(pkg: &mut SheetPackage, part: &str, name: &str, el: &str) {
        let xml = text(pkg, part);
        let tag = format!("name=\"{name}\"/>");
        let xml = xml.replacen(&tag, &format!("name=\"{name}\">{el}</tableColumn>"), 1);
        pkg.parts.iter_mut().find(|(n, _)| n == part).unwrap().1 = xml.into_bytes();
    }

    /// A row inserted at the first data row leaves a relative calculated
    /// formula reading the first data row in the saved part too.
    #[test]
    fn an_insert_at_the_first_data_row_saves_the_same_formula() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        resize_table(&mut pkg.workbook, "Table1", (0, 0, 2, 2)).unwrap();
        pkg.workbook.tables[0].set_calculated_formula(2, Some("B2*2".into()));
        crate::edit::insert_rows(&mut pkg.workbook, 0, 1, 1);
        assert_eq!(column_formula(&reload(&pkg), &part), "B2*2");
        crate::edit::delete_rows(&mut pkg.workbook, 0, 1, 1);
        crate::edit::delete_rows(&mut pkg.workbook, 0, 1, 1);
        assert_eq!(column_formula(&reload(&pkg), &part), "B2*2");
    }

    /// A header renamed after every data row went still renames the saved
    /// calculated formula.
    #[test]
    fn a_header_rename_in_a_header_only_table_saves_its_formula() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        resize_table(&mut pkg.workbook, "Table1", (0, 0, 2, 2)).unwrap();
        pkg.workbook.tables[0].set_calculated_formula(2, Some("[@Qty]*2".into()));
        crate::edit::delete_rows(&mut pkg.workbook, 0, 1, 2);
        pkg.workbook.sheets[0].set_cell(0, 1, Cell::text("Units"));
        crate::edit::sync_table_headers(&mut pkg.workbook, 0, &[(0, 1)]);
        assert_eq!(
            column_formula(&reload(&pkg), &part),
            "Table1[[#This Row],[Units]]*2"
        );
    }

    /// A sheet rename reaches the saved calculated formula.
    #[test]
    fn a_sheet_rename_reaches_the_saved_calculated_formula() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        pkg.add_sheet("Source");
        resize_table(&mut pkg.workbook, "Table1", (0, 0, 2, 2)).unwrap();
        pkg.workbook.tables[0].set_calculated_formula(2, Some("Source!$A$1".into()));
        crate::edit::rename_sheet(&mut pkg.workbook, 1, "Inputs");
        assert_eq!(column_formula(&reload(&pkg), &part), "Inputs!$A$1");
    }

    /// A formula the model clears leaves the part; an array formula, which
    /// the model doesn't hold, stays.
    #[test]
    fn a_cleared_calculated_formula_leaves_the_part() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        column_child(
            &mut pkg,
            &part,
            "Item",
            r#"<calculatedColumnFormula array="1">TRANSPOSE(Table1[Qty])</calculatedColumnFormula>"#,
        );
        calculated(&mut pkg, &part, "Qty", "LEN(Table1[[#This Row],[Item]])");
        let mut re = reload(&pkg);
        assert_eq!(re.workbook.tables[0].calculated_formulas[0], None);
        re.workbook.tables[0].set_calculated_formula(1, None);
        let again = reload(&re);
        let xml = text(&again, &part);
        assert!(!xml.contains("LEN("), "{xml}");
        assert!(
            xml.contains(r#"<calculatedColumnFormula array="1">"#),
            "{xml}"
        );
        assert!(
            again.workbook.tables[0]
                .calculated_formulas
                .iter()
                .all(Option::is_none)
        );
    }

    /// An empty formula element loads as none and saves as the file has it.
    #[test]
    fn an_empty_calculated_formula_element_saves_unchanged() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        column_child(&mut pkg, &part, "Item", "<calculatedColumnFormula/>");
        calculated(&mut pkg, &part, "Qty", "LEN(Table1[[#This Row],[Item]])");
        let re = reload(&pkg);
        assert_eq!(re.workbook.tables[0].calculated_formulas[0], None);
        assert_eq!(text(&reload(&re), &part), text(&pkg, &part));
    }

    /// A CDATA formula loads as its text and saves unchanged.
    #[test]
    fn a_cdata_calculated_formula_saves_unchanged() {
        let mut pkg = one_table();
        let part = pkg.workbook.tables[0].part.clone();
        column_child(
            &mut pkg,
            &part,
            "Qty",
            "<calculatedColumnFormula><![CDATA[LEN(A2)&\"<\"]]></calculatedColumnFormula>",
        );
        let re = reload(&pkg);
        assert_eq!(
            re.workbook.tables[0].calculated_formulas[1].as_deref(),
            Some("LEN(A2)&\"<\"")
        );
        assert_eq!(text(&reload(&re), &part), text(&pkg, &part));
    }

    /// Row and column edits move the A1 references of a calculated formula
    /// as they move the cells' formulas, and a deleted column takes its
    /// formula with it.
    #[test]
    fn calculated_formulas_follow_row_and_column_edits() {
        let mut pkg = one_table();
        pkg.workbook.sheets[0].set_cell(9, 5, Cell::number(2.0));
        resize_table(&mut pkg.workbook, "Table1", (0, 0, 2, 2)).unwrap();
        pkg.workbook.tables[0].set_calculated_formula(1, Some("$F$10".into()));
        pkg.workbook.tables[0].set_calculated_formula(2, Some("[@Qty]*$F$10".into()));
        crate::edit::insert_rows(&mut pkg.workbook, 0, 5, 1);
        assert_eq!(
            pkg.workbook.tables[0].calculated_formulas[2].as_deref(),
            Some("[@Qty]*$F$11")
        );
        crate::edit::delete_cols(&mut pkg.workbook, 0, 1, 1);
        let t = &pkg.workbook.tables[0];
        assert_eq!(t.columns, vec!["Item", "Column3"]);
        assert_eq!(
            t.calculated_formulas,
            vec![None, Some("#REF!*$E$11".to_string())]
        );
    }
}
