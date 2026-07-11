//! Pivot tables: parse the (byte-preserved) pivot parts read-only, and
//! refresh pivot output regions from current source data through the
//! columnar query core in [`crate::frame`].
//!
//! **Graceful degradation:** pivots using features we don't model — page
//! filters, hidden items, calculated fields, measures-on-rows — are marked
//! unsupported and never refreshed; their cached cells stay untouched. On
//! save every pivot cache gets `refreshOnLoad="1"`, so real Excel rebuilds
//! the layout from the same definition we computed from.

use opccore::xml::{Event, XmlParser};

use crate::formula::Value;
use crate::frame::{Agg, Frame, Measure, PivotSpec};
use crate::sheet::{CellValue, Workbook, parse_range_name};

/// Where a pivot cache reads its records from.
#[derive(Clone, Debug, PartialEq)]
pub enum PivotSource {
    /// A worksheet rect, first row = headers.
    Range {
        sheet: String,
        rect: (u32, u32, u32, u32),
    },
    /// An Excel Table (or defined name resolving to one).
    Table(String),
}

/// One data field: display name, cache-field index, aggregation.
#[derive(Clone, Debug)]
pub struct DataField {
    pub name: String,
    pub field: usize,
    pub agg: Agg,
}

/// A pivot table, as much of it as refresh needs.
#[derive(Clone, Debug)]
pub struct Pivot {
    pub name: String,
    /// Sheet the pivot output lives on.
    pub sheet: usize,
    /// Output region (r1, c1, r2, c2) from `<location ref>`; updated by
    /// refresh when the result grows or shrinks.
    pub location: (u32, u32, u32, u32),
    pub source: PivotSource,
    /// Cache field names, in cache order (field indices index this).
    pub fields: Vec<String>,
    pub row_fields: Vec<usize>,
    pub col_fields: Vec<usize>,
    pub data_fields: Vec<DataField>,
    pub grand_rows: bool,
    pub grand_cols: bool,
    /// Uses features refresh doesn't model — never refreshed.
    pub unsupported: bool,
    /// Part names, for save-time patching.
    pub part: String,
    pub cache_part: String,
}

/// Parse a pivotTableDefinition part. Returns the pivot (source/fields
/// still empty — filled from its cache) plus the cacheId to resolve.
pub(crate) fn parse_pivot_table_xml(xml: &str, sheet: usize, part: &str) -> Option<(Pivot, u32)> {
    let mut p = XmlParser::new(xml);
    let mut piv = Pivot {
        name: String::new(),
        sheet,
        location: (0, 0, 0, 0),
        source: PivotSource::Table(String::new()),
        fields: Vec::new(),
        row_fields: Vec::new(),
        col_fields: Vec::new(),
        data_fields: Vec::new(),
        grand_rows: true,
        grand_cols: true,
        unsupported: false,
        part: part.to_string(),
        cache_part: String::new(),
    };
    let mut cache_id = None;
    let mut got_location = false;
    #[derive(PartialEq)]
    enum Section {
        None,
        Rows,
        Cols,
    }
    let mut section = Section::None;
    loop {
        match p.next() {
            Event::Start => match local_name(p.name()) {
                "pivotTableDefinition" => {
                    piv.name = decode(p.attr("name"));
                    cache_id = p.attr("cacheId").parse::<u32>().ok();
                    if p.attr("rowGrandTotals") == "0" {
                        piv.grand_rows = false;
                    }
                    if p.attr("colGrandTotals") == "0" {
                        piv.grand_cols = false;
                    }
                    // Measures on rows would need a transposed layout.
                    if p.attr("dataOnRows") == "1" {
                        piv.unsupported = true;
                    }
                }
                "location" => {
                    if let Some(rect) = parse_range_name(p.attr("ref")) {
                        piv.location = rect;
                        got_location = true;
                    }
                }
                "rowFields" => section = Section::Rows,
                "colFields" => section = Section::Cols,
                "field" => {
                    if let Ok(x) = p.attr("x").parse::<i64>() {
                        match section {
                            // x = -2 is the "Values" pseudo-field: fine on
                            // columns (our layout), unsupported on rows.
                            Section::Rows if x == -2 => piv.unsupported = true,
                            Section::Rows if x >= 0 => piv.row_fields.push(x as usize),
                            Section::Cols if x >= 0 => piv.col_fields.push(x as usize),
                            _ => {}
                        }
                    }
                }
                "pageFields" | "pageField" => {
                    // Report filters can hide records; refresh would be wrong.
                    piv.unsupported = true;
                }
                "item" => {
                    // A hidden item is an active row/column filter.
                    if p.attr("h") == "1" {
                        piv.unsupported = true;
                    }
                }
                "dataField" => {
                    let fld = p.attr("fld").parse::<usize>().ok();
                    let agg = Agg::from_subtotal(p.attr("subtotal"));
                    match (fld, agg) {
                        (Some(field), Some(agg)) => {
                            let name = decode(p.attr("name"));
                            piv.data_fields.push(DataField { name, field, agg });
                        }
                        _ => piv.unsupported = true,
                    }
                }
                _ => {}
            },
            Event::End => {
                if matches!(local_name(p.name()), "rowFields" | "colFields") {
                    section = Section::None;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !got_location || piv.data_fields.is_empty() {
        piv.unsupported = true;
    }
    Some((piv, cache_id?))
}

/// Parse a pivotCacheDefinition part → (source, field names, unsupported).
pub(crate) fn parse_pivot_cache_xml(xml: &str) -> Option<(PivotSource, Vec<String>, bool)> {
    let mut p = XmlParser::new(xml);
    let mut source = None;
    let mut fields = Vec::new();
    let mut unsupported = false;
    loop {
        match p.next() {
            Event::Start => match local_name(p.name()) {
                "worksheetSource" => {
                    let name = decode(p.attr("name"));
                    if !name.is_empty() {
                        source = Some(PivotSource::Table(name));
                    } else if let Some(rect) = parse_range_name(p.attr("ref")) {
                        source = Some(PivotSource::Range {
                            sheet: decode(p.attr("sheet")),
                            rect,
                        });
                    }
                }
                "cacheSource" => {
                    if p.attr("type") != "worksheet" {
                        // External / consolidation sources.
                        unsupported = true;
                    }
                }
                "cacheField" => {
                    fields.push(decode(p.attr("name")));
                    // A calculated field has its formula on the cache field.
                    if !p.attr("formula").is_empty() {
                        unsupported = true;
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Some((source?, fields, unsupported))
}

/// What a refresh pass did.
#[derive(Debug, Default, PartialEq)]
pub struct RefreshOutcome {
    pub refreshed: usize,
    pub skipped: usize,
    /// Every cell whose stored value may have changed (old ∪ new region of
    /// each refreshed pivot) — feed these to the recalc engine.
    pub changed: Vec<(usize, u32, u32)>,
}

/// Recompute every supported pivot from current source data and write its
/// output region. Unsupported pivots keep their cached cells.
pub fn refresh_pivots(wb: &mut Workbook) -> RefreshOutcome {
    let mut out = RefreshOutcome::default();
    for i in 0..wb.pivots.len() {
        let p = wb.pivots[i].clone();
        if p.unsupported {
            out.skipped += 1;
            continue;
        }
        // Snapshot the source.
        let frame = match &p.source {
            PivotSource::Range { sheet, rect } => match wb.sheet_index(sheet) {
                Some(si) => Frame::from_range(wb, si, *rect),
                None => {
                    out.skipped += 1;
                    continue;
                }
            },
            PivotSource::Table(name) => match Frame::from_table(wb, name) {
                Some(f) => f,
                None => {
                    out.skipped += 1;
                    continue;
                }
            },
        };
        // Cache-field index → frame column, by name first (robust when the
        // source grew), cache order as fallback.
        let col_of = |fi: usize| -> Option<usize> {
            p.fields
                .get(fi)
                .and_then(|n| frame.col_index(n))
                .or_else(|| (fi < frame.cols.len()).then_some(fi))
        };
        let map_fields =
            |fs: &[usize]| -> Option<Vec<usize>> { fs.iter().map(|&f| col_of(f)).collect() };
        let (Some(rows), Some(cols)) = (map_fields(&p.row_fields), map_fields(&p.col_fields))
        else {
            out.skipped += 1;
            continue;
        };
        let measures: Option<Vec<Measure>> = p
            .data_fields
            .iter()
            .map(|df| {
                col_of(df.field).map(|col| Measure {
                    col,
                    agg: df.agg,
                    name: if df.name.is_empty() {
                        format!("{} of {}", df.agg.label(), frame.names[col])
                    } else {
                        df.name.clone()
                    },
                })
            })
            .collect();
        let Some(measures) = measures else {
            out.skipped += 1;
            continue;
        };
        let spec = PivotSpec {
            rows,
            cols,
            measures,
            filters: Vec::new(),
            grand_rows: p.grand_rows,
            grand_cols: p.grand_cols,
        };
        let result = crate::frame::pivot(&frame, &spec);

        // Write the grid at the location's top-left; clear whatever the old
        // region had beyond it.
        let (r1, c1, old_r2, old_c2) = p.location;
        let new_r2 = r1 + result.grid.len() as u32 - 1;
        let new_c2 = c1 + result.grid[0].len() as u32 - 1;
        let Some(sheet) = wb.sheets.get_mut(p.sheet) else {
            out.skipped += 1;
            continue;
        };
        for r in r1..=old_r2.max(new_r2) {
            for c in c1..=old_c2.max(new_c2) {
                let v = result
                    .grid
                    .get((r - r1) as usize)
                    .and_then(|row| row.get((c - c1) as usize))
                    .cloned()
                    .unwrap_or(Value::Empty);
                let style = sheet.cell(r, c).map(|cl| cl.style).unwrap_or(0);
                let value = match v {
                    Value::Empty => CellValue::Empty,
                    Value::Num(n) => CellValue::Number(n),
                    Value::Str(s) => CellValue::Text(s),
                    Value::Bool(b) => CellValue::Bool(b),
                    Value::Err(e) => CellValue::Error(e.code().to_string()),
                };
                sheet.set_cell(
                    r,
                    c,
                    crate::sheet::Cell {
                        value,
                        style,
                        ..crate::sheet::Cell::default()
                    },
                );
                out.changed.push((p.sheet, r, c));
            }
        }
        wb.pivots[i].location = (r1, c1, new_r2, new_c2);
        out.refreshed += 1;
    }
    out
}

fn local_name(name: &str) -> &str {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_items_and_page_fields_mark_unsupported() {
        let base = |extra: &str| {
            format!(
                r#"<pivotTableDefinition name="P" cacheId="1"><location ref="A1:B2"/><pivotFields>{extra}</pivotFields><rowFields><field x="0"/></rowFields><dataFields><dataField fld="1"/></dataFields></pivotTableDefinition>"#
            )
        };
        // Plain items: supported.
        let (p, id) = parse_pivot_table_xml(
            &base(r#"<pivotField axis="axisRow"><items><item x="0"/></items></pivotField>"#),
            0,
            "p",
        )
        .unwrap();
        assert_eq!(id, 1);
        assert!(!p.unsupported);
        assert_eq!(p.data_fields[0].agg, Agg::Sum); // subtotal default
        // A hidden item is an active filter → unsupported.
        let (p, _) = parse_pivot_table_xml(
            &base(r#"<pivotField axis="axisRow"><items><item x="0" h="1"/></items></pivotField>"#),
            0,
            "p",
        )
        .unwrap();
        assert!(p.unsupported);
        // Page (report-filter) fields → unsupported.
        let with_page = r#"<pivotTableDefinition name="P" cacheId="1"><location ref="A1:B2"/><pageFields count="1"><pageField fld="2"/></pageFields><rowFields><field x="0"/></rowFields><dataFields><dataField fld="1" subtotal="average"/></dataFields></pivotTableDefinition>"#;
        let (p, _) = parse_pivot_table_xml(with_page, 0, "p").unwrap();
        assert!(p.unsupported);
        assert_eq!(p.data_fields[0].agg, Agg::Average);
    }

    #[test]
    fn cache_parses_table_and_range_sources() {
        let range = r#"<pivotCacheDefinition><cacheSource type="worksheet"><worksheetSource ref="A1:C5" sheet="Data"/></cacheSource><cacheFields><cacheField name="A"/><cacheField name="B"/></cacheFields></pivotCacheDefinition>"#;
        let (src, fields, unsupported) = parse_pivot_cache_xml(range).unwrap();
        assert_eq!(
            src,
            PivotSource::Range {
                sheet: "Data".into(),
                rect: (0, 0, 4, 2)
            }
        );
        assert_eq!(fields, vec!["A", "B"]);
        assert!(!unsupported);
        let table = r#"<pivotCacheDefinition><cacheSource type="worksheet"><worksheetSource name="Sales"/></cacheSource><cacheFields><cacheField name="A"/></cacheFields></pivotCacheDefinition>"#;
        let (src, _, _) = parse_pivot_cache_xml(table).unwrap();
        assert_eq!(src, PivotSource::Table("Sales".into()));
        // Calculated fields poison refresh.
        let calc = r#"<pivotCacheDefinition><cacheSource type="worksheet"><worksheetSource ref="A1:B2" sheet="D"/></cacheSource><cacheFields><cacheField name="A" formula="B*2"/></cacheFields></pivotCacheDefinition>"#;
        let (_, _, unsupported) = parse_pivot_cache_xml(calc).unwrap();
        assert!(unsupported);
    }
}
