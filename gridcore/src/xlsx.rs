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

    // Shared strings + styles (relative to the workbook dir).
    let shared = rels
        .iter()
        .find(|(_, ty, _)| ty.ends_with("/sharedStrings"))
        .and_then(|(_, _, t)| get_str(&resolve(t)))
        .map(|xml| parse_shared_strings(&xml))
        .unwrap_or_default();
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
        let mut sheet = parse_worksheet(&xml, &shared);
        sheet.name = name;
        let sheet_idx = sheets.len();
        orig_to_model.push(Some(sheet_idx));

        // Excel Tables and pivot tables attached to this worksheet, via its
        // own rels part.
        let ws_dir = part.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let ws_file = part.rsplit_once('/').map(|(_, f)| f).unwrap_or(&part);
        let ws_rels_name = format!("{ws_dir}/_rels/{ws_file}.rels");
        if let Some(rels_xml) = get_str(&ws_rels_name) {
            for (_, ty, target) in parse_rels(&rels_xml) {
                if ty.ends_with("/table") {
                    let table_part = resolve_relative(ws_dir, &target);
                    if let Some(txml) = get_str(&table_part) {
                        if let Some(t) = parse_table_xml(&txml, sheet_idx, &table_part) {
                            tables.push(t);
                        }
                    }
                } else if ty.ends_with("/pivotTable") {
                    pending_pivots.push((sheet_idx, resolve_relative(ws_dir, &target)));
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
        match cache
            .and_then(|(part, xml)| crate::pivot::parse_pivot_cache_xml(&xml).map(|c| (part, c)))
        {
            Some((cache_part, (source, fields, cache_unsupported))) => {
                piv.cache_part = cache_part;
                piv.source = source;
                piv.fields = fields;
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
    }
    let mut numfmts: BTreeMap<u32, NumFmt> = BTreeMap::new();
    let mut codes: BTreeMap<u32, String> = BTreeMap::new();
    let mut fonts: Vec<Font> = Vec::new();
    let mut fills: Vec<Option<(u8, u8, u8)>> = Vec::new();
    let mut xfs: Vec<Xf> = Vec::new();

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
                "b" => {
                    if let Some(f) = &mut cur_font {
                        f.bold = p.attr("val") != "0" && p.attr("val") != "false";
                    }
                }
                "i" => {
                    if let Some(f) = &mut cur_font {
                        f.italic = p.attr("val") != "0" && p.attr("val") != "false";
                    }
                }
                "color" => {
                    if let Some(f) = &mut cur_font {
                        f.color = parse_rgb(p.attr("rgb"));
                    }
                }
                "fills" => in_fills = true,
                "fill" if in_fills => cur_fill = Some(None),
                "fgColor" => {
                    if let Some(fl) = &mut cur_fill {
                        *fl = parse_rgb(p.attr("rgb"));
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
                    });
                }
                "alignment" if in_cellxfs => {
                    if let Some(x) = xfs.last_mut() {
                        x.align = crate::sheet::Align::from_attr(p.attr("horizontal"));
                    }
                }
                _ => {}
            },
            Event::End => match local(p.name()) {
                "fonts" => in_fonts = false,
                "font" => {
                    if let Some(f) = cur_font.take() {
                        fonts.push(f);
                    }
                }
                "fills" => in_fills = false,
                "fill" => {
                    if let Some(fl) = cur_fill.take() {
                        fills.push(fl);
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
    Styles { xfs }
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
    let mut next_numfmt = max_numfmt_id(orig) + 1;

    let (mut new_fonts, mut new_fills, mut new_numfmts, mut new_xfs) =
        (String::new(), String::new(), String::new(), String::new());
    let (mut fonts_added, mut fills_added, mut numfmts_added) = (0u32, 0u32, 0u32);

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
        font.push_str("<sz val=\"11\"/><name val=\"Calibri\"/></font>");
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
            "<xf numFmtId=\"{num_id}\" fontId=\"{font_id}\" fillId=\"{fill_id}\" borderId=\"0\" xfId=\"0\" applyFont=\"1\""
        );
        if apply_num {
            x.push_str(" applyNumberFormat=\"1\"");
        }
        if apply_fill {
            x.push_str(" applyFill=\"1\"");
        }
        match xf.align.attr() {
            Some(a) => x.push_str(&format!(
                " applyAlignment=\"1\"><alignment horizontal=\"{a}\"/></xf>"
            )),
            None => x.push_str("/>"),
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
    xml = bump_count(&xml, "<cellXfs", authored.len() as u32);
    xml = xml.replacen("</cellXfs>", &format!("{new_xfs}</cellXfs>"), 1);
    xml
}

/// One worksheet: `<sheetData>`, `<cols>`, `<mergeCells>`; everything else is
/// preserved through the source-splice on save.
fn parse_worksheet(xml: &str, shared: &[String]) -> Sheet {
    let mut sheet = Sheet::default();
    let mut p = XmlParser::new(xml);

    // Shared-formula masters: si → (row, col, source).
    let mut shared_masters: BTreeMap<u32, (u32, u32, String)> = BTreeMap::new();
    // Followers to fill in after the pass: (row, col, si).
    let mut followers: Vec<(u32, u32, u32)> = Vec::new();

    let mut cur_row: u32 = 0;
    let mut next_col: u32 = 0;
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
                            attrs.push_str(a.value);
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
                            attrs.push_str(a.value);
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
                _ => {}
            },
            Event::End => {
                if local(p.name()) == "row" {
                    cur_row += 1;
                }
            }
            Event::Eof => break,
            _ => {}
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
                                    attrs.push_str(a.value);
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

pub(crate) fn esc_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Full-precision float for `<v>` (must round-trip; display formatting is a
/// separate concern).
fn num_repr(n: f64) -> String {
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
    let sst_name = "xl/sharedStrings.xml";
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
            let xml = String::from_utf8_lossy(&p.1).into_owned();
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
            _ => out.push(ch),
        }
    }
    out
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
            grand_rows: true,
            grand_cols: true,
            subtotals: false,
            unsupported: false,
            edited: true,
            part: table_part,
            cache_part,
        });
        Some(self.workbook.pivots.len() - 1)
    }

    /// Remove the sheet at `idx`. Returns false (and does nothing) when it
    /// is the last sheet — a workbook must keep at least one.
    pub fn remove_sheet(&mut self, idx: usize) -> bool {
        if self.workbook.sheets.len() <= 1 || idx >= self.workbook.sheets.len() {
            return false;
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
        workbook: Workbook {
            sheets: vec![Sheet {
                name: "Sheet1".to_string(),
                ..Sheet::default()
            }],
            styles: Styles {
                xfs: vec![Xf::default()],
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
    use super::*;

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
        pkg.workbook.sheets[0].set_cell(0, 0, crate::sheet::Cell::formula("SEQUENCE(3)"));
        let mut eng = crate::engine::Engine::new(&pkg.workbook);
        eng.recalc_all(&mut pkg.workbook);
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
}
