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

use std::collections::BTreeMap;

use opccore::xml::{Event, XmlParser};
use opccore::zip::ZipArchive;
use opccore::zipwrite::write_zip;

use crate::formula::translate_formula;
use crate::sheet::{
    Cell, CellValue, ColDef, DefinedName, NumFmt, Sheet, Styles, Table, Workbook, Xf, cell_name,
    classify_builtin, classify_format_code, parse_cell_name, parse_range_name,
};

const OLE2: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XlsxError {
    /// Not a ZIP container at all.
    NotZip,
    /// An OLE2 compound file — the legacy binary `.xls` format.
    LegacyXls,
    CorruptPart,
    MissingWorkbook,
    NotUtf8,
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
        })
    }
}

impl std::error::Error for XlsxError {}

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

    /// The raw bytes of a part by name.
    pub fn part(&self, name: &str) -> Option<&[u8]> {
        self.parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
    }
}

// ---------------------------------------------------------------------------
// Load
// ---------------------------------------------------------------------------

/// Open an `.xlsx` from bytes, keeping all parts for a lossless-ish save.
pub fn load_xlsx(data: &[u8]) -> Result<SheetPackage, XlsxError> {
    let zip = match ZipArchive::open(data) {
        Some(z) => z,
        None => {
            if data.len() >= 8 && data[..8] == OLE2 {
                return Err(XlsxError::LegacyXls);
            }
            return Err(XlsxError::NotZip);
        }
    };
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    for e in zip.entries() {
        let bytes = zip.extract(e).ok_or(XlsxError::CorruptPart)?;
        parts.push((e.name.clone(), bytes));
    }

    let get = |name: &str| {
        parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
    };
    let get_str = |name: &str| get(name).map(|b| String::from_utf8_lossy(b).into_owned());

    // Locate the workbook part via the package rels (virtually always
    // xl/workbook.xml, but resolve it properly).
    let wb_part = get_str("_rels/.rels")
        .and_then(|xml| {
            parse_rels(&xml)
                .into_iter()
                .find(|(_, ty, _)| ty.ends_with("/officeDocument"))
                .map(|(_, _, target)| target.trim_start_matches('/').to_string())
        })
        .unwrap_or_else(|| "xl/workbook.xml".to_string());
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
    let resolve = |target: &str| -> String {
        if let Some(abs) = target.strip_prefix('/') {
            abs.to_string()
        } else if wb_dir.is_empty() {
            target.to_string()
        } else {
            format!("{wb_dir}/{target}")
        }
    };

    // Workbook: sheet list + date system + defined names.
    let (sheet_meta, date1904, iterate, raw_names) = parse_workbook_xml(&wb_xml);

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
    // localSheetId counts workbook.xml order; map it to model indices in
    // case a sheet part is missing and gets skipped.
    let mut orig_to_model: Vec<Option<usize>> = Vec::new();
    for (name, rid) in sheet_meta {
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
        let sheet_idx = sheets.len();
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

    Ok(SheetPackage {
        parts,
        sheet_parts,
        shared,
        shared_part,
        workbook: Workbook {
            sheets,
            styles,
            defined_names,
            tables,
            pivots,
            date1904,
            iterate,
        },
    })
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
                "tableColumn" => columns.push(decode(p.attr("name"))),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Some(Table {
        name,
        sheet: sheet_idx,
        range: range?,
        header_rows,
        totals_rows,
        columns,
        part: part.to_string(),
    })
}

/// Parse a `.rels` stream into (id, type, target) triples.
pub(crate) fn parse_rels(xml: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == "Relationship" => {
                out.push((
                    decode(p.attr("Id")),
                    decode(p.attr("Type")),
                    decode(p.attr("Target")),
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
    Vec<(String, String)>,
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
                    sheets.push((name, rid));
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
                        // Skip Excel's internal names (print areas etc.).
                        if !n.0.starts_with("_xlnm.") && !n.2.is_empty() {
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
        size: Option<f64>,
        name: Option<String>,
    }
    let mut numfmts: BTreeMap<u32, NumFmt> = BTreeMap::new();
    let mut codes: BTreeMap<u32, String> = BTreeMap::new();
    let mut fonts: Vec<Font> = Vec::new();
    let mut fills: Vec<Option<(u8, u8, u8)>> = Vec::new();
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
    let mut cur_fill: Option<Option<(u8, u8, u8)>> = None;
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
                    } else if dxf_in_font {
                        if let Some(d) = &mut cur_dxf {
                            d.color = parse_rgb(p.attr("rgb"));
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
                "fill" if in_fills => cur_fill = Some(None),
                // dxf solid fills carry the colour in `<bgColor>` (or `<fgColor>`).
                "bgColor" | "fgColor" => {
                    if let Some(fl) = &mut cur_fill {
                        if p.name().ends_with("fgColor") {
                            *fl = parse_rgb(p.attr("rgb"));
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
                    xfs.push(Xf {
                        numfmt,
                        code,
                        bold: font.bold,
                        italic: font.italic,
                        color: font.color,
                        fill: fills.get(fill_id).copied().flatten(),
                        align: crate::sheet::Align::General,
                        font_size: font.size,
                        font_name: font.name.clone(),
                        border: false,
                        wrap: false,
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

/// Append the authored `xfs` (with fresh fonts/fills/numFmts) to the original
/// `styles.xml`, leaving every existing style byte-for-byte intact.
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

    for xf in authored {
        // Font (always minted so the id is exact).
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
        let font_id = font_base + fonts_added;
        new_fonts.push_str(&font);
        fonts_added += 1;

        // Fill (only when a background is set).
        let (fill_id, apply_fill) = if let Some((r, g, b)) = xf.fill {
            new_fills.push_str(&format!(
                "<fill><patternFill patternType=\"solid\"><fgColor rgb=\"FF{r:02X}{g:02X}{b:02X}\"/><bgColor indexed=\"64\"/></patternFill></fill>"
            ));
            let id = fill_base + fills_added;
            fills_added += 1;
            (id, true)
        } else {
            (0, false)
        };

        // Border (a thin box around the cell) when set.
        let (border_id, apply_border) = if xf.border {
            new_borders.push_str(
                "<border><left style=\"thin\"/><right style=\"thin\"/><top style=\"thin\"/><bottom style=\"thin\"/><diagonal/></border>",
            );
            let id = border_base + borders_added;
            borders_added += 1;
            (id, true)
        } else {
            (0, false)
        };

        // Number format (custom code only).
        let (num_id, apply_num) = if let Some(code) = &xf.code {
            let id = next_numfmt;
            next_numfmt += 1;
            numfmts_added += 1;
            new_numfmts.push_str(&format!(
                "<numFmt numFmtId=\"{id}\" formatCode=\"{}\"/>",
                esc_attr(code)
            ));
            (id, true)
        } else {
            (0, false)
        };

        let mut x = format!(
            "<xf numFmtId=\"{num_id}\" fontId=\"{font_id}\" fillId=\"{fill_id}\" borderId=\"{border_id}\" xfId=\"0\" applyFont=\"1\""
        );
        if apply_num {
            x.push_str(" applyNumberFormat=\"1\"");
        }
        if apply_fill {
            x.push_str(" applyFill=\"1\"");
        }
        if apply_border {
            x.push_str(" applyBorder=\"1\"");
        }
        let horiz = xf.align.attr();
        if horiz.is_some() || xf.wrap {
            x.push_str(" applyAlignment=\"1\"><alignment");
            if let Some(a) = horiz {
                x.push_str(&format!(" horizontal=\"{a}\""));
            }
            if xf.wrap {
                x.push_str(" wrapText=\"1\"");
            }
            x.push_str("/></xf>");
        } else {
            x.push_str("/>");
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
    let mut in_cf_formula = false;
    let mut cf_formula_buf = String::new();

    // Data-validation parse state.
    let mut cur_dv: Option<crate::sheet::DataValidation> = None;
    let mut dv_formula: u8 = 0; // 0 = none, 1 = formula1, 2 = formula2
    let mut dv_buf = String::new();

    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "col" => {
                    let min: u32 = p.attr("min").parse().unwrap_or(1);
                    let max: u32 = p.attr("max").parse().unwrap_or(min);
                    let width = p.attr("width").parse::<f64>().ok();
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
                    let cell = parse_cell_body(
                        &mut p,
                        &ctype,
                        style,
                        shared,
                        row,
                        col,
                        &mut shared_masters,
                        &mut followers,
                    );
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
                "pane" => {
                    if matches!(p.attr("state"), "frozen" | "frozenSplit") {
                        let cols = p.attr("xSplit").parse::<u32>().unwrap_or(0);
                        let rows = p.attr("ySplit").parse::<u32>().unwrap_or(0);
                        sheet.freeze = (rows, cols);
                    }
                }
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
                    cur_cf = Some(CondFormat {
                        ranges,
                        rules: Vec::new(),
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
                    cur_dv = Some(crate::sheet::DataValidation {
                        ranges,
                        kind: p.attr("type").to_string(),
                        operator: p.attr("operator").to_string(),
                        formula1: String::new(),
                        formula2: String::new(),
                        prompt,
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
                "formula" if in_cf_formula => {
                    in_cf_formula = false;
                    cf_formulas.push(std::mem::take(&mut cf_formula_buf));
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
                            _ => CfKind::Other,
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
                    if let Some(dv) = cur_dv.take() {
                        if !dv.ranges.is_empty() && dv.is_meaningful() {
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
    sheet
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

    // An array formula's `ref` records its spill extent (dynamic arrays and
    // legacy CSE alike); the engine re-derives it on recalculation.
    let spill = f_attrs.as_deref().and_then(|a| {
        if !a.contains("t=\"array\"") {
            return None;
        }
        let ref_val = a.split("ref=\"").nth(1)?.split('"').next()?;
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
    }
}

/// Local name (strip any namespace prefix).
fn local(name: &str) -> &str {
    match name.rfind(':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

fn decode(raw: &str) -> String {
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

/// Serialize the package back to `.xlsx` bytes (STORED ZIP).
pub fn save_xlsx(pkg: &SheetPackage) -> Vec<u8> {
    let mut parts = pkg.parts.clone();
    let wb = &pkg.workbook;

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
        let sheet_data = sheet_data_xml(sheet, &mut index_of, &mut any_formulas);
        let updated = splice_worksheet(&source, sheet, &sheet_data);
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
                        p.1 = chart_space_xml(cd).into_bytes();
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
                    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"{total}\" uniqueCount=\"{total}\">{additions}</sst>"
                );
                parts.push((sst_name.to_string(), xml.into_bytes()));
                add_content_type_override(
                    &mut parts,
                    "/xl/sharedStrings.xml",
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml",
                );
                add_workbook_rel(
                    &mut parts,
                    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings",
                    "sharedStrings.xml",
                );
            }
        }
    }

    // --- table geometry: row edits move table ranges; the part must follow --
    for t in &wb.tables {
        let Some(p) = parts.iter_mut().find(|(n, _)| n == &t.part) else {
            continue;
        };
        let xml = String::from_utf8_lossy(&p.1).into_owned();
        let (r1, c1, r2, c2) = t.range;
        let full = format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2));
        let mut updated = patch_ref_attr(&xml, "<table", &full);
        // autoFilter covers the table minus its totals row.
        let af_r2 = r2.saturating_sub(t.totals_rows).max(r1);
        let af = format!("{}:{}", cell_name(r1, c1), cell_name(af_r2, c2));
        updated = patch_ref_attr(&updated, "<autoFilter", &af);
        p.1 = updated.into_bytes();
    }

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
            if let crate::pivot::PivotSource::Range { sheet, .. } = &piv.source {
                xml = patch_worksheet_source_sheet(&xml, sheet);
            }
            p.1 = set_refresh_on_load(&xml).into_bytes();
        }
    }

    // --- sheet names: the model is authoritative ----------------------------
    // workbook.xml is otherwise preserved verbatim, so a rename in the model
    // must be patched into the <sheet name="…"> attributes (in order).
    if let Some(p) = parts.iter_mut().find(|(n, _)| n == "xl/workbook.xml") {
        let xml = String::from_utf8_lossy(&p.1).into_owned();
        p.1 = patch_sheet_names(&xml, &wb.sheets).into_bytes();
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

    write_zip(&parts)
}

/// `<sheetData>` for one sheet: rows in order, preserved row attrs, cells
/// with values/formulas/styles.
fn sheet_data_xml(
    sheet: &Sheet,
    index_of: &mut impl FnMut(&str) -> usize,
    any_formulas: &mut bool,
) -> String {
    let mut out = String::from("<sheetData>");
    // Union of rows that have cells or preserved attributes.
    let mut rows: Vec<u32> = sheet.cells.keys().map(|&(r, _)| r).collect();
    rows.extend(sheet.row_attrs.keys().copied());
    rows.sort_unstable();
    rows.dedup();

    for &row in &rows {
        let attrs = sheet.row_attrs.get(&row).map(|s| s.as_str()).unwrap_or("");
        let cells: Vec<(&(u32, u32), &Cell)> =
            sheet.cells.range((row, 0)..=(row, u32::MAX)).collect();
        if cells.is_empty() {
            out.push_str(&format!("<row r=\"{}\"{attrs}/>", row + 1));
            continue;
        }
        out.push_str(&format!("<row r=\"{}\"{attrs}>", row + 1));
        for (&(r, c), cell) in cells {
            out.push_str(&cell_xml(r, c, cell, index_of, any_formulas));
        }
        out.push_str("</row>");
    }
    out.push_str("</sheetData>");
    out
}

fn cell_xml(
    row: u32,
    col: u32,
    cell: &Cell,
    index_of: &mut impl FnMut(&str) -> usize,
    any_formulas: &mut bool,
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
    let (t_attr, body) = match &cell.value {
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

    let f_xml = match (&cell.formula, &cell.f_attrs) {
        // A spilling anchor writes fresh array attributes — its extent may
        // have changed since load, so any stored ref would be stale.
        (Some(src), _) if cell.spill.is_some() && !src.is_empty() => {
            let (h, w) = cell.spill.unwrap();
            format!(
                "<f t=\"array\" ref=\"{}:{}\">{}</f>",
                cell_name(row, col),
                cell_name(row + h - 1, col + w - 1),
                esc_text(src)
            )
        }
        (Some(src), None) => format!("<f>{}</f>", esc_text(src)),
        (Some(src), Some(fa)) if src.is_empty() => format!("<f{fa}/>"),
        (Some(src), Some(fa)) => format!("<f{fa}>{}</f>", esc_text(src)),
        (None, _) => String::new(),
    };

    if body.is_empty() && f_xml.is_empty() {
        format!("<c{attrs}/>")
    } else {
        format!("<c{attrs}{t_attr}>{f_xml}{body}</c>")
    }
}

/// Byte offset of a real `<tag` element start in XML, skipping any occurrence
/// inside a `<!-- … -->` comment and requiring the tag name to be followed by
/// whitespace, `>`, or `/` (so `<sheetDataX` and a `<sheetData` literal buried
/// in a comment don't misdirect the splice). `None` if not present.
fn find_element(hay: &str, tag: &str) -> Option<usize> {
    let needle = format!("<{tag}");
    let bytes = hay.as_bytes();
    let mut i = 0;
    while i < hay.len() {
        // Skip over comments wholesale.
        if hay[i..].starts_with("<!--") {
            // Unterminated comment: nothing usable after it.
            let rel = hay[i..].find("-->")?;
            i += rel + 3;
            continue;
        }
        if hay[i..].starts_with(&needle) {
            let after = bytes.get(i + needle.len()).copied();
            if matches!(
                after,
                None | Some(b'>')
                    | Some(b'/')
                    | Some(b' ')
                    | Some(b'\t')
                    | Some(b'\r')
                    | Some(b'\n')
            ) {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Replace `<sheetData>…</sheetData>` (or `<sheetData/>`) in the original
/// worksheet XML, refresh `<dimension>`, and regenerate `<cols>`.
fn splice_worksheet(source: &str, sheet: &Sheet, sheet_data: &str) -> String {
    let mut out = match find_element(source, "sheetData") {
        Some(start) => {
            let after = &source[start..];
            let gt = after
                .find('>')
                .map(|i| start + i)
                .unwrap_or(source.len() - 1);
            let end = if source[..gt + 1].ends_with("/>") {
                gt + 1
            } else {
                source[gt..]
                    .find("</sheetData>")
                    .map(|i| gt + i + "</sheetData>".len())
                    .unwrap_or(source.len())
            };
            format!("{}{}{}", &source[..start], sheet_data, &source[end..])
        }
        None => {
            // Degenerate worksheet with no sheetData: put ours before the
            // closing tag.
            source.replacen("</worksheet>", &format!("{sheet_data}</worksheet>"), 1)
        }
    };

    // <dimension ref="…"/> → recomputed used range.
    let (rows, cols) = sheet.used_size();
    let dim = if rows == 0 {
        "A1".to_string()
    } else {
        format!("A1:{}", cell_name(rows - 1, cols.max(1) - 1))
    };
    if let Some(i) = find_element(&out, "dimension") {
        if let Some(rel) = out[i..].find("ref=\"") {
            let vs = i + rel + 5;
            if let Some(ve) = out[vs..].find('"') {
                out.replace_range(vs..vs + ve, &dim);
            }
        }
    }

    // <cols> — regenerate from the model when we have definitions.
    if !sheet.col_defs.is_empty() {
        let mut cols_xml = String::from("<cols>");
        for d in &sheet.col_defs {
            let width = match d.width {
                Some(w) => format!(" width=\"{w}\" customWidth=\"1\""),
                None => String::new(),
            };
            cols_xml.push_str(&format!(
                "<col min=\"{}\" max=\"{}\"{width}{}/>",
                d.min + 1,
                d.max + 1,
                d.attrs
            ));
        }
        cols_xml.push_str("</cols>");
        if let Some(start) = find_element(&out, "cols") {
            let end = out[start..]
                .find("</cols>")
                .map(|i| start + i + "</cols>".len())
                .or_else(|| out[start..].find("/>").map(|i| start + i + 2))
                .unwrap_or(start);
            out.replace_range(start..end, &cols_xml);
        } else if let Some(start) = find_element(&out, "sheetData") {
            out.insert_str(start, &cols_xml);
        }
    }

    // Frozen panes: sync the first sheetView's <pane> from the model.
    let out = set_freeze_pane(&out, sheet.freeze);
    // Merged regions: regenerate <mergeCells> from the model.
    let out = set_merge_cells(&out, &sheet.merges);
    // Sheet protection: <sheetProtection> right after </sheetData> (schema order
    // puts it ahead of mergeCells/conditionalFormatting).
    set_sheet_protection(&out, sheet.protection.as_deref())
}

/// Sync the `<sheetProtection>` element from the model: drop any existing one,
/// then re-insert right after `</sheetData>` when the sheet is protected.
/// Idempotent.
fn set_sheet_protection(xml: &str, attrs: Option<&str>) -> String {
    // Drop any existing self-closing <sheetProtection .../> first.
    let mut out = if let Some(s) = xml.find("<sheetProtection") {
        let e = xml[s..].find("/>").map(|i| s + i + 2).unwrap_or(s);
        format!("{}{}", &xml[..s], &xml[e..])
    } else {
        xml.to_string()
    };
    if let Some(attrs) = attrs {
        let block = format!("<sheetProtection {attrs}/>");
        let pos = out
            .find("</sheetData>")
            .map(|i| i + "</sheetData>".len())
            .or_else(|| out.find("</worksheet>"))
            .unwrap_or(out.len());
        out.insert_str(pos, &block);
    }
    out
}

/// Rewrite the `<mergeCells>` block from the model's merged regions (removing it
/// when there are none). Placed right after `</sheetData>` (schema order).
/// Idempotent.
fn set_merge_cells(xml: &str, merges: &[(u32, u32, u32, u32)]) -> String {
    // Drop any existing <mergeCells>…</mergeCells> (or self-closing) first.
    let mut out = if let Some(s) = xml.find("<mergeCells") {
        let e = xml[s..]
            .find("</mergeCells>")
            .map(|i| s + i + "</mergeCells>".len())
            .or_else(|| xml[s..].find("/>").map(|i| s + i + 2))
            .unwrap_or(s);
        format!("{}{}", &xml[..s], &xml[e..])
    } else {
        xml.to_string()
    };
    if merges.is_empty() {
        return out;
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
    // After </sheetData>, else before </worksheet>.
    if let Some(pos) = out.find("</sheetData>").map(|i| i + "</sheetData>".len()) {
        out.insert_str(pos, &block);
    } else {
        out = out.replacen("</worksheet>", &format!("{block}</worksheet>"), 1);
    }
    out
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
    // Drop any existing <pane …/> first (idempotent; also handles unfreeze).
    let mut out = if let Some(ps) = xml.find("<pane") {
        let pe = xml[ps..]
            .find("/>")
            .map(|i| ps + i + 2)
            .or_else(|| xml[ps..].find("</pane>").map(|i| ps + i + "</pane>".len()))
            .unwrap_or(ps);
        format!("{}{}", &xml[..ps], &xml[pe..])
    } else {
        xml.to_string()
    };
    if pane.is_empty() {
        return out;
    }
    // Insert into the first <sheetView> (expanding a self-closing one).
    if let Some(sv) = find_element(&out, "sheetView") {
        if let Some(gt) = out[sv..].find('>').map(|i| sv + i) {
            if out.as_bytes()[gt - 1] == b'/' {
                out = format!("{}>{}</sheetView>{}", &out[..gt - 1], pane, &out[gt + 1..]);
            } else {
                out.insert_str(gt + 1, &pane);
            }
            return out;
        }
    }
    // No <sheetView>: insert a full block after <dimension …>, else after the
    // <worksheet …> opening tag (both keep the schema's element order).
    let block =
        format!("<sheetViews><sheetView workbookViewId=\"0\">{pane}</sheetView></sheetViews>");
    let anchor = find_element(&out, "dimension")
        .and_then(|d| {
            out[d..]
                .find("/>")
                .map(|i| d + i + 2)
                .or_else(|| out[d..].find('>').map(|i| d + i + 1))
        })
        .or_else(|| {
            out.find("<worksheet")
                .and_then(|w| out[w..].find('>').map(|i| w + i + 1))
        });
    if let Some(pos) = anchor {
        out.insert_str(pos, &block);
    }
    out
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

/// The chart kinds [`chart_space_xml`] can author. Everything else — scatter,
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

/// A self-contained `chartSpace` for the kinds `chart_kind_is_writable`
/// accepts (bar, column, line, pie). Each series and the categories are written
/// as a `strRef`/`numRef` naming the cells they read plus a cache of what those
/// cells said, so the chart stays live in Excel; a series with no reference
/// falls back to literals (`strLit`/`numLit`) and renders without a source
/// range. `pub(crate)` so sibling modules' tests can round-trip parse → write →
/// parse.
pub(crate) fn chart_space_xml(data: &crate::sheet::ChartData) -> String {
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
<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<c:chart><c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>{}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val=\"0\"/></c:title>\
<c:autoTitleDeleted val=\"0\"/><c:plotArea><c:layout/>\
{plot_body}{axes}\
</c:plotArea><c:legend><c:legendPos val=\"b\"/><c:overlay val=\"0\"/></c:legend><c:plotVisOnly val=\"1\"/><c:dispBlanksAs val=\"gap\"/></c:chart></c:chartSpace>",
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
fn remove_element_containing(xml: &str, prefix: &str, needle: &str) -> String {
    let mut search_from = 0;
    while let Some(rel) = xml[search_from..].find(prefix) {
        let start = search_from + rel;
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

/// Guarantee `<calcPr … fullCalcOnLoad="1"/>` in workbook.xml.
fn ensure_full_calc(xml: &str) -> String {
    if let Some(i) = xml.find("<calcPr") {
        if xml[i..].starts_with("<calcPr")
            && xml[i..xml[i..].find('>').map(|g| i + g).unwrap_or(xml.len())]
                .contains("fullCalcOnLoad")
        {
            return xml.to_string();
        }
        let mut out = xml.to_string();
        out.insert_str(i + "<calcPr".len(), " fullCalcOnLoad=\"1\"");
        out
    } else {
        xml.replacen(
            "</workbook>",
            "<calcPr calcId=\"0\" fullCalcOnLoad=\"1\"/></workbook>",
            1,
        )
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
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"{SPREADSHEET_NS}\"><dimension ref=\"A1\"/><sheetData/></worksheet>"
        );
        self.parts.push((part_name.clone(), body.into_bytes()));
        add_content_type_override(
            &mut self.parts,
            &format!("/{part_name}"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
        );
        let rid = add_workbook_rel(
            &mut self.parts,
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet",
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

    /// Write a clustered column chart (cached literal data, self-contained) onto
    /// `sheet`, anchored over the cell rect `from`..`to`, wiring the full OPC:
    /// the chart part, a drawing part with a twoCellAnchor graphicFrame, both
    /// rels, the content-type overrides, and the worksheet's `<drawing>` element.
    /// Also registers it in the model so it round-trips on reload.
    /// Add a `cellIs` conditional-formatting rule (Excel's "Highlight Cells"):
    /// the differential format `dxf` applies to `range` when the cell value
    /// satisfies `op` (greaterThan / lessThan / between / equal / …) against
    /// `formula1` (and `formula2` for `between`). Wires the OPC (a `<dxf>` in
    /// styles.xml + a `<conditionalFormatting>` in the worksheet) and the model,
    /// so it renders via cf::cell_dxf and round-trips.
    pub fn add_conditional_format(
        &mut self,
        sheet: usize,
        range: (u32, u32, u32, u32),
        op: &str,
        formula1: &str,
        formula2: Option<&str>,
        dxf: crate::sheet::Dxf,
    ) {
        if sheet >= self.workbook.sheets.len() {
            return;
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
        // Worksheet: inject <conditionalFormatting> after </sheetData> (mergeCells,
        // if any, is spliced in ahead of it at save time, keeping schema order).
        let (r1, c1, r2, c2) = range;
        let sqref = format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2));
        let mut fmls = format!("<formula>{}</formula>", esc_text(formula1));
        if let Some(f2) = formula2 {
            fmls.push_str(&format!("<formula>{}</formula>", esc_text(f2)));
        }
        let cf_xml = format!(
            "<conditionalFormatting sqref=\"{sqref}\"><cfRule type=\"cellIs\" dxfId=\"{dxf_id}\" priority=\"{priority}\" operator=\"{op}\">{fmls}</cfRule></conditionalFormatting>"
        );
        let sheet_part = self.sheet_parts[sheet].clone();
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == sheet_part) {
            let mut xml = String::from_utf8_lossy(&p.1).into_owned();
            if let Some(pos) = xml.find("</sheetData>").map(|i| i + "</sheetData>".len()) {
                xml.insert_str(pos, &cf_xml);
            } else {
                xml = xml.replacen("</worksheet>", &format!("{cf_xml}</worksheet>"), 1);
            }
            p.1 = xml.into_bytes();
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
        self.workbook.sheets[sheet]
            .cond_formats
            .push(crate::sheet::CondFormat {
                ranges: vec![range],
                rules: vec![rule],
            });
    }

    /// Add a data-validation rule to `sheet` over `range`. For a list, pass
    /// kind="list" and formula1 as an inline `"a,b,c"` list or a range ref;
    /// numeric/date kinds use an operator (between/greaterThan/…) + operand(s).
    /// Appends a `<dataValidation>` to the worksheet (preserving existing ones)
    /// and the model, so it round-trips and drives the dropdown / cell check.
    pub fn add_data_validation(
        &mut self,
        sheet: usize,
        range: (u32, u32, u32, u32),
        kind: &str,
        operator: &str,
        formula1: &str,
        formula2: Option<&str>,
    ) {
        if sheet >= self.workbook.sheets.len() {
            return;
        }
        let (r1, c1, r2, c2) = range;
        let sqref = format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2));
        let op_attr = if operator.is_empty() {
            String::new()
        } else {
            format!(" operator=\"{operator}\"")
        };
        let mut fmls = format!("<formula1>{}</formula1>", esc_text(formula1));
        if let Some(f2) = formula2 {
            fmls.push_str(&format!("<formula2>{}</formula2>", esc_text(f2)));
        }
        let dv_xml = format!(
            "<dataValidation type=\"{kind}\"{op_attr} allowBlank=\"1\" showInputMessage=\"1\" showErrorMessage=\"1\" sqref=\"{sqref}\">{fmls}</dataValidation>"
        );
        let sheet_part = self.sheet_parts[sheet].clone();
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == sheet_part) {
            let mut xml = String::from_utf8_lossy(&p.1).into_owned();
            if let Some(s) = xml.find("<dataValidations") {
                // Bump the count attribute, then append before </dataValidations>.
                if let Some(cs) = xml[s..].find("count=\"").map(|i| s + i + 7) {
                    if let Some(ce) = xml[cs..].find('"').map(|i| cs + i) {
                        if let Ok(n) = xml[cs..ce].parse::<u32>() {
                            xml.replace_range(cs..ce, &(n + 1).to_string());
                        }
                    }
                }
                if let Some(e) = xml.find("</dataValidations>") {
                    xml.insert_str(e, &dv_xml);
                }
            } else {
                let block = format!("<dataValidations count=\"1\">{dv_xml}</dataValidations>");
                // dataValidations follows sheetData/mergeCells/conditionalFormatting;
                // insert before the first following element (hyperlinks/pageMargins/…).
                let anchor = ["<hyperlinks", "<pageMargins", "<drawing", "<legacyDrawing"]
                    .iter()
                    .filter_map(|t| xml.find(t))
                    .min()
                    .or_else(|| xml.find("</worksheet>"));
                if let Some(pos) = anchor {
                    xml.insert_str(pos, &block)
                }
            }
            p.1 = xml.into_bytes();
        }
        self.workbook.sheets[sheet]
            .validations
            .push(crate::sheet::DataValidation {
                ranges: vec![range],
                kind: kind.to_string(),
                operator: operator.to_string(),
                formula1: formula1.to_string(),
                formula2: formula2.unwrap_or("").to_string(),
                prompt: None,
            });
    }

    /// Create an Excel Table ("Format as Table") over `range`. Column names come
    /// from the header row when `has_header` (else "Column1"…). Wires the OPC (a
    /// `xl/tables/table*.xml` part, its content type, a worksheet `/table` rel +
    /// `<tableParts>`) and the model, so it round-trips and Excel styles it with
    /// the given `style` (e.g. "TableStyleMedium2"). Returns the table index.
    pub fn add_table(
        &mut self,
        sheet: usize,
        range: (u32, u32, u32, u32),
        has_header: bool,
        style: &str,
    ) -> Option<usize> {
        if sheet >= self.workbook.sheets.len() {
            return None;
        }
        let (r1, c1, r2, c2) = range;
        let mut tn = 1;
        while self.part(&format!("xl/tables/table{tn}.xml")).is_some() {
            tn += 1;
        }
        // Column names: from the header row (deduped), else generated.
        let mut names: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for c in c1..=c2 {
            let base = if has_header {
                match self.workbook.sheets[sheet].cell(r1, c).map(|cl| &cl.value) {
                    Some(crate::sheet::CellValue::Text(t)) if !t.trim().is_empty() => t.clone(),
                    Some(crate::sheet::CellValue::Number(n)) => n.to_string(),
                    _ => format!("Column{}", c - c1 + 1),
                }
            } else {
                format!("Column{}", c - c1 + 1)
            };
            let (mut nm, mut k) = (base.clone(), 1);
            while !seen.insert(nm.clone()) {
                k += 1;
                nm = format!("{base}{k}");
            }
            names.push(nm);
        }
        // Unique table display name.
        let mut k = self.workbook.tables.len() + 1;
        let name = loop {
            let cand = format!("Table{k}");
            if !self.workbook.tables.iter().any(|t| t.name == cand) {
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
        let table_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<table xmlns=\"{SPREADSHEET_NS}\" id=\"{tn}\" name=\"{name}\" displayName=\"{name}\" ref=\"{sqref}\"{header_attr} totalsRowShown=\"0\"><autoFilter ref=\"{sqref}\"/><tableColumns count=\"{}\">{cols_xml}</tableColumns><tableStyleInfo name=\"{style}\" showFirstColumn=\"0\" showLastColumn=\"0\" showRowStripes=\"1\" showColumnStripes=\"0\"/></table>",
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
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/table",
            &format!("../tables/table{tn}.xml"),
        );
        if !rid.is_empty() {
            if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == sheet_part) {
                let mut xml = String::from_utf8_lossy(&p.1).into_owned();
                if !xml.contains("xmlns:r=") {
                    xml = xml.replacen("<worksheet ", "<worksheet xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" ", 1);
                }
                let entry = format!("<tablePart r:id=\"{rid}\"/>");
                if let Some(s) = xml.find("<tableParts") {
                    // Bump count + append before </tableParts>.
                    if let Some(cs) = xml[s..].find("count=\"").map(|i| s + i + 7) {
                        if let Some(ce) = xml[cs..].find('"').map(|i| cs + i) {
                            if let Ok(cnt) = xml[cs..ce].parse::<u32>() {
                                xml.replace_range(cs..ce, &(cnt + 1).to_string());
                            }
                        }
                    }
                    if let Some(e) = xml.find("</tableParts>") {
                        xml.insert_str(e, &entry);
                    }
                } else {
                    let block = format!("<tableParts count=\"1\">{entry}</tableParts>");
                    let pos = xml
                        .find("<extLst")
                        .unwrap_or_else(|| xml.find("</worksheet>").unwrap_or(xml.len()));
                    xml.insert_str(pos, &block);
                }
                p.1 = xml.into_bytes();
            }
        }
        self.workbook.tables.push(crate::sheet::Table {
            name,
            sheet,
            range,
            header_rows: u32::from(has_header),
            totals_rows: 0,
            columns: names,
            part,
        });
        Some(self.workbook.tables.len() - 1)
    }

    /// Remove all conditional-formatting rules from `sheet` (model + the
    /// worksheet's `<conditionalFormatting>` elements). Orphaned `<dxf>`s are left
    /// in styles.xml — harmless and referenced by nothing.
    pub fn clear_conditional_formats(&mut self, sheet: usize) {
        if sheet >= self.workbook.sheets.len() {
            return;
        }
        self.workbook.sheets[sheet].cond_formats.clear();
        let sheet_part = self.sheet_parts[sheet].clone();
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == sheet_part) {
            let mut xml = String::from_utf8_lossy(&p.1).into_owned();
            while let Some(s) = xml.find("<conditionalFormatting") {
                let e = xml[s..]
                    .find("</conditionalFormatting>")
                    .map(|i| s + i + "</conditionalFormatting>".len())
                    .or_else(|| xml[s..].find("/>").map(|i| s + i + 2))
                    .unwrap_or(xml.len());
                xml.replace_range(s..e, "");
            }
            p.1 = xml.into_bytes();
        }
    }

    pub fn add_chart(
        &mut self,
        sheet: usize,
        from: (u32, u32),
        to: (u32, u32),
        data: &crate::sheet::ChartData,
    ) {
        if sheet >= self.workbook.sheets.len() {
            return;
        }
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
        let host = self.workbook.sheets[sheet]
            .drawing_part
            .clone()
            .filter(|p| self.part(p).is_some());
        let drawing_part = host.clone().unwrap_or_else(|| {
            let mut dn = 1;
            while self.part(&format!("xl/drawings/drawing{dn}.xml")).is_some() {
                dn += 1;
            }
            format!("xl/drawings/drawing{dn}.xml")
        });
        let (d_dir, d_file) = drawing_part
            .rsplit_once('/')
            .unwrap_or(("", drawing_part.as_str()));
        let (d_dir, d_file) = (d_dir.to_string(), d_file.to_string());

        // 2) drawing rels → chart (its rId names the chart from the anchor).
        // Minted BEFORE the chart part is written, so a failure here leaves no
        // orphan behind.
        let d_rels = format!("{d_dir}/_rels/{d_file}.rels");
        let c_target = relative_target(&d_dir, &chart_part);
        let c_rid = match add_rel(
            &mut self.parts,
            &d_rels,
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart",
            &c_target,
        ) {
            // "" means the rel was already there — a dangling one naming a chart
            // part the zip doesn't hold, so `cn` picked its name. Reuse its id;
            // an anchor written with `r:id=""` makes Excel call the whole
            // workbook unreadable.
            id if id.is_empty() => rel_id_for(&self.parts, &d_rels, &c_target).unwrap_or_default(),
            id => id,
        };
        if c_rid.is_empty() {
            return; // no id to point the graphic frame at; leave the file alone
        }

        // 3) chart part + content type.
        self.parts
            .push((chart_part.clone(), chart_space_xml(data).into_bytes()));
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
            "<{px}twoCellAnchor xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<{px}from><{px}col>{fc}</{px}col><{px}colOff>0</{px}colOff><{px}row>{fr}</{px}row><{px}rowOff>0</{px}rowOff></{px}from>\
<{px}to><{px}col>{tc}</{px}col><{px}colOff>0</{px}colOff><{px}row>{tr}</{px}row><{px}rowOff>0</{px}rowOff></{px}to>\
<{px}graphicFrame macro=\"\"><{px}nvGraphicFramePr><{px}cNvPr id=\"{id}\" name=\"Chart {cn}\"/><{px}cNvGraphicFramePr/></{px}nvGraphicFramePr>\
<{px}xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></{px}xfrm>\
<a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" r:id=\"{c_rid}\"/></a:graphicData></a:graphic></{px}graphicFrame>\
<{px}clientData/></{px}twoCellAnchor>",
            id = shape_id
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
                    None => return,
                }
            }
            None => {
                let drawing_xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
{anchor}</xdr:wsDr>"
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
        let rid = match add_rel(
            &mut self.parts,
            &rels_part,
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing",
            &target,
        ) {
            // "" means the rel was already there; reuse its id.
            id if id.is_empty() => rel_id_for(&self.parts, &rels_part, &target).unwrap_or_default(),
            id => id,
        };
        if !rid.is_empty() {
            if let Some(p) = self.parts.iter_mut().find(|(n, _)| *n == sheet_part) {
                let mut xml = String::from_utf8_lossy(&p.1).into_owned();
                if !xml.contains("xmlns:r=") {
                    xml = xml.replacen("<worksheet ", "<worksheet xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" ", 1);
                }
                if !xml.contains("<drawing ") {
                    // CT_Worksheet is a SEQUENCE: `drawing` comes before
                    // `legacyDrawing`, `picture`, `oleObjects`, `controls`,
                    // `tableParts` and `extLst`. Appending at `</worksheet>`
                    // puts it after any of those, and Excel treats an
                    // out-of-order child as unreadable content — it "repairs"
                    // the file by dropping the drawing or the table.
                    let pos = [
                        "<legacyDrawingHF",
                        "<legacyDrawing",
                        "<drawingHF",
                        "<picture",
                        "<oleObjects",
                        "<controls",
                        "<webPublishItems",
                        "<tableParts",
                        "<extLst",
                        "</worksheet>",
                    ]
                    .iter()
                    .filter_map(|t| xml.find(t))
                    .min()
                    .unwrap_or(xml.len());
                    xml.insert_str(pos, &format!("<drawing r:id=\"{rid}\"/>"));
                } else if host.is_none() {
                    // The worksheet names a drawing the LOADER rejected — a rel
                    // that resolves to nothing, or a part missing from the zip —
                    // so the model had none and we just minted a fresh one. A
                    // worksheet may carry only one `<drawing>`, so leaving the
                    // stale element in place orphans the part we wrote: the
                    // chart the user inserted would be silently absent from the
                    // saved file. Point the element at the new part instead.
                    if let Some(at) = xml.find("<drawing ") {
                        let gt = xml[at..].find('>').map(|i| at + i + 1).unwrap_or(xml.len());
                        let end = if xml[at..gt].ends_with("/>") {
                            gt
                        } else {
                            // `<drawing …></drawing>`: take the close tag too,
                            // or it would be left dangling.
                            xml[gt..]
                                .find("</drawing>")
                                .map(|i| gt + i + "</drawing>".len())
                                .unwrap_or(gt)
                        };
                        xml.replace_range(at..end, &format!("<drawing r:id=\"{rid}\"/>"));
                    }
                }
                p.1 = xml.into_bytes();
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
        let mut cache_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<pivotCacheDefinition xmlns=\"{SPREADSHEET_NS}\" refreshOnLoad=\"1\" recordCount=\"0\"><cacheSource type=\"worksheet\">{source_xml}</cacheSource><cacheFields count=\"{}\">",
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
        let cache_rid = add_workbook_rel(
            &mut self.parts,
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition",
            &format!("pivotCache/pivotCacheDefinition{m}.xml"),
        );

        // workbook.xml: register the cache.
        if let Some(p) = self
            .parts
            .iter_mut()
            .find(|(pn, _)| pn == "xl/workbook.xml")
        {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            let entry = format!("<pivotCache cacheId=\"{cache_id}\" r:id=\"{cache_rid}\"/>");
            p.1 = if xml.contains("</pivotCaches>") {
                xml.replacen("</pivotCaches>", &format!("{entry}</pivotCaches>"), 1)
            } else {
                xml.replacen(
                    "</sheets>",
                    &format!("</sheets><pivotCaches>{entry}</pivotCaches>"),
                    1,
                )
            }
            .into_bytes();
        }

        // The pivot definition. Save rewrites the field layout (the pivot is
        // registered as edited), so this base only needs valid structure.
        let (lr, lc) = location;
        let loc_ref = format!("{}:{}", cell_name(lr, lc), cell_name(lr + 1, lc + 1));
        let mut table_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<pivotTableDefinition xmlns=\"{SPREADSHEET_NS}\" name=\"PivotTable{n}\" cacheId=\"{cache_id}\" dataCaption=\"Values\" useAutoFormatting=\"1\" indent=\"0\" outline=\"1\" outlineData=\"1\"><location ref=\"{loc_ref}\" firstHeaderRow=\"1\" firstDataRow=\"1\" firstDataCol=\"1\"/><pivotFields count=\"{}\">",
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
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable",
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
                p.1 = remove_element_containing(
                    &xml,
                    "<pivotCache",
                    &format!("r:id=\"{cache_rid}\""),
                )
                .into_bytes();
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
        self.parts.retain(|(n, _)| *n != part_name);
        // Content-type override for the removed part.
        if let Some(p) = self
            .parts
            .iter_mut()
            .find(|(n, _)| n == "[Content_Types].xml")
        {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            p.1 =
                remove_element_containing(&xml, "<Override", &format!("/{part_name}")).into_bytes();
        }
        // Relationship (by target) — capture its rId first.
        let target = part_name.trim_start_matches("xl/").to_string();
        let mut rid = String::new();
        if let Some(p) = self
            .parts
            .iter_mut()
            .find(|(n, _)| n == "xl/_rels/workbook.xml.rels")
        {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            if let Some(rel_pos) = xml.find(&format!("Target=\"{target}\"")) {
                if let Some(id_pos) = xml[..rel_pos].rfind("Id=\"") {
                    let s = id_pos + 4;
                    if let Some(e) = xml[s..].find('\"') {
                        rid = xml[s..s + e].to_string();
                    }
                }
            }
            p.1 = remove_element_containing(&xml, "<Relationship", &format!("Target=\"{target}\""))
                .into_bytes();
        }
        // workbook.xml: drop the <sheet> element and fix defined-name scopes
        // (localSheetId counts sheets in document order).
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == "xl/workbook.xml") {
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

/// A fresh single-sheet workbook (the "create new" path and a save target for
/// in-memory workbooks).
pub fn new_xlsx() -> SheetPackage {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/></Types>"#;
    let root_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{RELS_NS}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#
    );
    let workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{SPREADSHEET_NS}" xmlns:r="{RELS_NS}"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let wb_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{RELS_NS}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="{RELS_NS}/styles" Target="styles.xml"/><Relationship Id="rId3" Type="{RELS_NS}/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#
    );
    let worksheet = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{SPREADSHEET_NS}"><dimension ref="A1"/><sheetViews><sheetView workbookViewId="0"/></sheetViews><sheetData/></worksheet>"#
    );
    // Minimal but Excel-complete styles: two fills (none + gray125) are
    // mandatory; one font, one border, one xf.
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{SPREADSHEET_NS}"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs><cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles></styleSheet>"#
    );
    let sst = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="0" uniqueCount="0"></sst>"#;

    let parts = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), root_rels.into_bytes()),
        ("xl/workbook.xml".to_string(), workbook.into_bytes()),
        (
            "xl/_rels/workbook.xml.rels".to_string(),
            wb_rels.into_bytes(),
        ),
        (
            "xl/worksheets/sheet1.xml".to_string(),
            worksheet.into_bytes(),
        ),
        ("xl/styles.xml".to_string(), styles.into_bytes()),
        ("xl/sharedStrings.xml".to_string(), sst.as_bytes().to_vec()),
    ];

    SheetPackage {
        parts,
        sheet_parts: vec!["xl/worksheets/sheet1.xml".to_string()],
        shared: Vec::new(),
        shared_part: Some("xl/sharedStrings.xml".to_string()),
        workbook: Workbook {
            sheets: vec![Sheet {
                name: "Sheet1".to_string(),
                ..Sheet::default()
            }],
            styles: Styles {
                xfs: vec![Xf::default()],
                ..Default::default()
            },
            defined_names: Vec::new(),
            tables: Vec::new(),
            pivots: Vec::new(),
            date1904: false,
            iterate: None,
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
            color: None,
            bold: Some(true),
            italic: None,
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
        // Defined names: real ones load, built-in _xlnm ones are skipped.
        assert_eq!(wb.defined_names.len(), 1);
        assert_eq!(wb.defined_name("total", 0), Some("Data!$B$2"));
        // Column width + row attrs + merges.
        assert_eq!(s.col_width(1), 20.0);
        assert!(s.row_attrs.get(&0).unwrap().contains("customHeight"));
        assert_eq!(s.merges, vec![(4, 0, 5, 1)]);
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
        assert_eq!(anchor.formula.as_deref(), Some("SEQUENCE(3)"));
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
}
