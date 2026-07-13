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
    /// Per cache field, the enumerated shared-item values (empty for numeric
    /// range fields). Filled from the cache; used to resolve item indices.
    pub field_items: Vec<Vec<Value>>,
    /// Active row/column filters: per pivot field, the shared-item indices that
    /// are hidden (`<item h="1" x="…"/>`). Those records are excluded on refresh.
    pub hidden: Vec<(usize, Vec<usize>)>,
    /// Report (page) filters: per `<pageField>`, the selected shared-item index,
    /// or `None` for "(All)". A selected item keeps only matching records.
    pub page: Vec<(usize, Option<usize>)>,
    /// Per pivot field, the shared index (`x`) of each `<item>` in order — maps a
    /// page field's `item` position to a shared index. Empty = identity.
    pub items_order: Vec<Vec<i64>>,
    /// Calculated fields: `(cache field index, formula)` — evaluated at refresh
    /// over each pivot cell's group sums.
    pub calc_formulas: Vec<(usize, String)>,
    pub grand_rows: bool,
    pub grand_cols: bool,
    /// Subtotal rows for outer row fields (Excel's default with two or
    /// more row fields, unless every row field opts out).
    pub subtotals: bool,
    /// Uses features refresh doesn't model — never refreshed.
    pub unsupported: bool,
    /// Field layout changed in the editor — save must rewrite the
    /// definition part (not just patch the location).
    pub edited: bool,
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
        field_items: Vec::new(),
        hidden: Vec::new(),
        page: Vec::new(),
        items_order: Vec::new(),
        calc_formulas: Vec::new(),
        grand_rows: true,
        grand_cols: true,
        subtotals: false,
        unsupported: false,
        edited: false,
        part: part.to_string(),
        cache_part: String::new(),
    };
    let mut cache_id = None;
    let mut got_location = false;
    let mut pivot_field_idx: i64 = -1;
    let mut no_subtotal: Vec<usize> = Vec::new();
    // The current pivot field's items (`x` per position) and hidden shared
    // indices, flushed onto `piv` when the next field starts / at the end.
    let mut cur_items: Vec<i64> = Vec::new();
    let mut cur_hidden: Vec<usize> = Vec::new();
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
                "pivotField" => {
                    flush_pivot_field(&mut piv, pivot_field_idx, &mut cur_items, &mut cur_hidden);
                    pivot_field_idx += 1;
                    if p.attr("defaultSubtotal") == "0" {
                        no_subtotal.push(pivot_field_idx as usize);
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
                // A report filter: keep records matching the selected item (a
                // real item index) or, for "(All)", don't filter.
                "pageField" => {
                    if let Ok(fld) = p.attr("fld").parse::<usize>() {
                        let sel = p
                            .attr("item")
                            .parse::<i64>()
                            .ok()
                            .filter(|&i| (0..32767).contains(&i))
                            .map(|i| i as usize);
                        piv.page.push((fld, sel));
                    }
                }
                // One `<item>` of the current pivot field. `t` (e.g. "default")
                // marks the subtotal placeholder — a slot but not a data value.
                "item" => {
                    let pos = cur_items.len();
                    let is_special = !p.attr("t").is_empty();
                    let x = p.attr("x").parse::<i64>().unwrap_or(pos as i64);
                    cur_items.push(if is_special { -1 } else { x });
                    if !is_special && x >= 0 && p.attr("h") == "1" {
                        cur_hidden.push(x as usize);
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
    flush_pivot_field(&mut piv, pivot_field_idx, &mut cur_items, &mut cur_hidden);
    if !got_location || piv.data_fields.is_empty() {
        piv.unsupported = true;
    }
    // Excel shows subtotals by default when row fields nest, unless every
    // row field has them switched off.
    piv.subtotals =
        piv.row_fields.len() >= 2 && piv.row_fields.iter().any(|f| !no_subtotal.contains(f));
    Some((piv, cache_id?))
}

/// Record the just-parsed pivot field's item order (`x` per position) at its
/// field index, and any hidden shared indices, then reset the scratch buffers.
fn flush_pivot_field(piv: &mut Pivot, idx: i64, items: &mut Vec<i64>, hidden: &mut Vec<usize>) {
    if idx < 0 {
        items.clear();
        hidden.clear();
        return;
    }
    let idx = idx as usize;
    while piv.items_order.len() <= idx {
        piv.items_order.push(Vec::new());
    }
    piv.items_order[idx] = std::mem::take(items);
    let h = std::mem::take(hidden);
    if !h.is_empty() {
        piv.hidden.push((idx, h));
    }
}

/// The parsed pieces of a pivotCacheDefinition: source, field names, per-field
/// shared-item values, calculated-field formulas, and whether it uses something
/// refresh can't model.
pub(crate) type CacheInfo = (
    PivotSource,
    Vec<String>,
    Vec<Vec<Value>>,
    Vec<(usize, String)>,
    bool,
);

/// Parse a pivotCacheDefinition part. `date1904` sets the epoch for `<d>` items.
pub(crate) fn parse_pivot_cache_xml(xml: &str, date1904: bool) -> Option<CacheInfo> {
    let mut p = XmlParser::new(xml);
    let mut source = None;
    let mut fields = Vec::new();
    let mut field_items: Vec<Vec<Value>> = Vec::new();
    let mut calc_formulas: Vec<(usize, String)> = Vec::new();
    let mut cur: Vec<Value> = Vec::new();
    let mut in_shared = false;
    let mut started = false;
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
                    // Flush the previous field's items, then start this one.
                    if started {
                        field_items.push(std::mem::take(&mut cur));
                    }
                    started = true;
                    fields.push(decode(p.attr("name")));
                    // A calculated field carries its formula (evaluated on refresh).
                    let formula = p.attr("formula");
                    if !formula.is_empty() {
                        calc_formulas.push((fields.len() - 1, decode(formula)));
                    }
                }
                "sharedItems" => in_shared = true,
                t @ ("s" | "n" | "b" | "d" | "e" | "m") if in_shared => {
                    cur.push(shared_item_value(t, p.attr("v"), date1904));
                }
                _ => {}
            },
            Event::End => {
                if local_name(p.name()) == "sharedItems" {
                    in_shared = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if started {
        field_items.push(cur);
    }
    Some((source?, fields, field_items, calc_formulas, unsupported))
}

/// One `<sharedItems>` value element → a cell value (dates → serial).
fn shared_item_value(tag: &str, v: &str, date1904: bool) -> Value {
    match tag {
        "s" => Value::Str(decode(v)),
        "n" => Value::Num(v.parse().unwrap_or(0.0)),
        "b" => Value::Bool(matches!(v, "1" | "true" | "True")),
        "d" => date_to_serial(v, date1904).map(Value::Num).unwrap_or(Value::Empty),
        "e" => Value::Str(decode(v)),
        _ => Value::Empty, // "m" = missing/blank
    }
}

/// Parse an ISO `YYYY-MM-DDThh:mm:ss` (time optional) into an Excel serial.
fn date_to_serial(s: &str, date1904: bool) -> Option<f64> {
    let (date, time) = s.split_once('T').unwrap_or((s, "00:00:00"));
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let m: u32 = dp.next()?.parse().ok()?;
    let d: u32 = dp.next()?.parse().ok()?;
    let mut tp = time.split(':');
    let hh: u32 = tp.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let mm: u32 = tp.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let ss: u32 = tp.next().and_then(|x| x.trim_end_matches('Z').parse().ok()).unwrap_or(0);
    Some(crate::sheet::parts_to_serial(y, m, d, hh * 3600 + mm * 60 + ss, date1904))
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
        // Calculated-field formula lookups for the calc parser.
        let calc_of = |name: &str| -> Option<String> {
            p.calc_formulas.iter().find_map(|(fi, f)| {
                (p.fields.get(*fi).map(|n| n.eq_ignore_ascii_case(name)) == Some(true))
                    .then(|| f.clone())
            })
        };
        let base_of = |name: &str| frame.col_index(name);
        let mut measures: Vec<Measure> = Vec::new();
        let mut ok = true;
        for df in &p.data_fields {
            let calc = p.calc_formulas.iter().find(|(fi, _)| *fi == df.field);
            let name = || {
                if df.name.is_empty() {
                    p.fields.get(df.field).cloned().unwrap_or_default()
                } else {
                    df.name.clone()
                }
            };
            if let Some((_, formula)) = calc {
                // A calculated field: parse its expression over base fields.
                match crate::pivotcalc::parse(formula, &base_of, &calc_of) {
                    Some(expr) => measures.push(Measure {
                        col: 0,
                        agg: df.agg,
                        name: name(),
                        calc: Some(expr),
                    }),
                    None => {
                        ok = false;
                        break;
                    }
                }
            } else {
                match col_of(df.field) {
                    Some(col) => measures.push(Measure {
                        col,
                        agg: df.agg,
                        name: if df.name.is_empty() {
                            format!("{} of {}", df.agg.label(), frame.names[col])
                        } else {
                            df.name.clone()
                        },
                        calc: None,
                    }),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
        }
        if !ok {
            out.skipped += 1;
            continue;
        }
        // Build record filters from hidden items (keep the non-hidden values) and
        // page fields (keep the selected item). If any index can't be resolved to
        // a value/column, fall back to skipping (keep the cached cells).
        let mut filters: Vec<(usize, Vec<Value>)> = Vec::new();
        let mut resolvable = true;
        for (fld, hidden_idx) in &p.hidden {
            match (col_of(*fld), p.field_items.get(*fld)) {
                (Some(col), Some(vals)) if !vals.is_empty() => {
                    let hidden: std::collections::HashSet<usize> =
                        hidden_idx.iter().copied().collect();
                    let allowed: Vec<Value> = vals
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !hidden.contains(i))
                        .map(|(_, v)| v.clone())
                        .collect();
                    filters.push((col, allowed));
                }
                _ => {
                    resolvable = false;
                    break;
                }
            }
        }
        if resolvable {
            for (fld, sel) in &p.page {
                let Some(item) = sel else { continue }; // "(All)"
                // A page field's `item` indexes the field's item order → shared idx.
                let shared = p
                    .items_order
                    .get(*fld)
                    .and_then(|order| order.get(*item))
                    .copied()
                    .unwrap_or(*item as i64);
                if shared < 0 {
                    continue; // default / "(All)" slot
                }
                match (col_of(*fld), p.field_items.get(*fld).and_then(|v| v.get(shared as usize))) {
                    (Some(col), Some(val)) => filters.push((col, vec![val.clone()])),
                    _ => {
                        resolvable = false;
                        break;
                    }
                }
            }
        }
        if !resolvable {
            out.skipped += 1;
            continue;
        }
        let spec = PivotSpec {
            rows,
            cols,
            measures,
            filters,
            grand_rows: p.grand_rows,
            grand_cols: p.grand_cols,
            subtotals: p.subtotals,
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

/// Rewrite an edited pivot's definition XML: regenerate the field layout
/// (`pivotFields`/`rowFields`/`colFields`/`dataFields`) from the model and
/// drop the stale cached layout (`rowItems`/`colItems`). Everything else —
/// location, styles, formats — is preserved.
pub fn rewrite_pivot_definition(xml: &str, p: &Pivot) -> String {
    let mut out = xml.to_string();
    for tag in [
        "pivotFields",
        "rowFields",
        "rowItems",
        "colFields",
        "colItems",
        "dataFields",
    ] {
        out = remove_block(&out, tag);
    }

    let mut ins = String::new();
    // pivotFields: one entry per cache field, in cache order.
    ins.push_str(&format!("<pivotFields count=\"{}\">", p.fields.len()));
    for i in 0..p.fields.len() {
        let mut attrs = String::new();
        if p.row_fields.contains(&i) {
            attrs.push_str(" axis=\"axisRow\"");
        } else if p.col_fields.contains(&i) {
            attrs.push_str(" axis=\"axisCol\"");
        }
        if p.data_fields.iter().any(|d| d.field == i) {
            attrs.push_str(" dataField=\"1\"");
        }
        ins.push_str(&format!("<pivotField{attrs} showAll=\"0\"/>"));
    }
    ins.push_str("</pivotFields>");
    if !p.row_fields.is_empty() {
        ins.push_str(&format!("<rowFields count=\"{}\">", p.row_fields.len()));
        for &i in &p.row_fields {
            ins.push_str(&format!("<field x=\"{i}\"/>"));
        }
        ins.push_str("</rowFields>");
    }
    // With several measures Excel places the "Values" pseudo-field (x = -2)
    // on the column axis — matching our measures-innermost layout.
    let values_on_cols = p.data_fields.len() > 1;
    if !p.col_fields.is_empty() || values_on_cols {
        let n = p.col_fields.len() + usize::from(values_on_cols);
        ins.push_str(&format!("<colFields count=\"{n}\">"));
        for &i in &p.col_fields {
            ins.push_str(&format!("<field x=\"{i}\"/>"));
        }
        if values_on_cols {
            ins.push_str("<field x=\"-2\"/>");
        }
        ins.push_str("</colFields>");
    }
    if !p.data_fields.is_empty() {
        ins.push_str(&format!("<dataFields count=\"{}\">", p.data_fields.len()));
        for df in &p.data_fields {
            let sub = df
                .agg
                .subtotal_code()
                .map(|c| format!(" subtotal=\"{c}\""))
                .unwrap_or_default();
            ins.push_str(&format!(
                "<dataField name=\"{}\" fld=\"{}\" baseField=\"0\" baseItem=\"0\"{sub}/>",
                xml_escape(&df.name),
                df.field
            ));
        }
        ins.push_str("</dataFields>");
    }

    // Insert right after the location element (schema order).
    if let Some(pos) = element_end(&out, "location") {
        out.insert_str(pos, &ins);
    }
    out
}

/// Byte offset just past the end of the first `<tag …/>` or `<tag …>…</tag>`.
fn element_end(xml: &str, tag: &str) -> Option<usize> {
    let start = xml.find(&format!("<{tag}"))?;
    let gt = start + xml[start..].find('>')?;
    if xml[..gt].ends_with('/') {
        return Some(gt + 1);
    }
    let close = format!("</{tag}>");
    let end = gt + xml[gt..].find(&close)?;
    Some(end + close.len())
}

/// Remove the first `<tag …/>` or `<tag …>…</tag>` block, if present.
fn remove_block(xml: &str, tag: &str) -> String {
    let Some(start) = xml.find(&format!("<{tag}")) else {
        return xml.to_string();
    };
    let Some(end) = element_end(xml, tag) else {
        return xml.to_string();
    };
    format!("{}{}", &xml[..start], &xml[end..])
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
    fn hidden_items_and_page_fields_are_parsed() {
        let base = |extra: &str| {
            format!(
                r#"<pivotTableDefinition name="P" cacheId="1"><location ref="A1:B2"/><pivotFields>{extra}</pivotFields><rowFields><field x="0"/></rowFields><dataFields><dataField fld="1"/></dataFields></pivotTableDefinition>"#
            )
        };
        // Plain items: supported, no filter.
        let (p, id) = parse_pivot_table_xml(
            &base(r#"<pivotField axis="axisRow"><items><item x="0"/></items></pivotField>"#),
            0,
            "p",
        )
        .unwrap();
        assert_eq!(id, 1);
        assert!(!p.unsupported && p.hidden.is_empty());
        assert_eq!(p.data_fields[0].agg, Agg::Sum); // subtotal default
        // A hidden item records the field + hidden shared index (still supported).
        let (p, _) = parse_pivot_table_xml(
            &base(
                r#"<pivotField axis="axisRow"><items><item x="0" h="1"/><item x="1"/></items></pivotField>"#,
            ),
            0,
            "p",
        )
        .unwrap();
        assert!(!p.unsupported);
        assert_eq!(p.hidden, vec![(0, vec![0])]);
        // A page field records the selected item (absent = "(All)").
        let with_page = r#"<pivotTableDefinition name="P" cacheId="1"><location ref="A1:B2"/><pageFields count="1"><pageField fld="2" item="3"/></pageFields><rowFields><field x="0"/></rowFields><dataFields><dataField fld="1" subtotal="average"/></dataFields></pivotTableDefinition>"#;
        let (p, _) = parse_pivot_table_xml(with_page, 0, "p").unwrap();
        assert!(!p.unsupported);
        assert_eq!(p.page, vec![(2, Some(3))]);
        assert_eq!(p.data_fields[0].agg, Agg::Average);
    }

    #[test]
    fn rewrite_regenerates_field_layout() {
        let xml = r#"<?xml version="1.0"?><pivotTableDefinition name="P" cacheId="1"><location ref="A3:B6" firstHeaderRow="1" firstDataRow="1" firstDataCol="1"/><pivotFields count="3"><pivotField axis="axisRow"><items count="2"><item x="0"/><item t="default"/></items></pivotField><pivotField/><pivotField dataField="1"/></pivotFields><rowFields count="1"><field x="0"/></rowFields><rowItems count="2"><i><x/></i><i t="grand"><x/></i></rowItems><dataFields count="1"><dataField name="Sum of Sales" fld="2"/></dataFields><pivotTableStyleInfo name="PivotStyleLight16"/></pivotTableDefinition>"#;
        let (mut piv, _) = parse_pivot_table_xml(xml, 0, "p").unwrap();
        piv.fields = vec!["Region".into(), "Product".into(), "Sales".into()];
        // Edit: rows = Product, cols = Region, value = Average of Sales.
        piv.row_fields = vec![1];
        piv.col_fields = vec![0];
        piv.data_fields = vec![DataField {
            name: "Average of Sales".into(),
            field: 2,
            agg: Agg::Average,
        }];
        piv.edited = true;
        let out = rewrite_pivot_definition(xml, &piv);
        assert!(out.contains(r#"<rowFields count="1"><field x="1"/></rowFields>"#));
        assert!(out.contains(r#"<colFields count="1"><field x="0"/></colFields>"#));
        assert!(out.contains(
            r#"<dataField name="Average of Sales" fld="2" baseField="0" baseItem="0" subtotal="average"/>"#
        ));
        assert!(out.contains(r#"<pivotField axis="axisCol" showAll="0"/>"#));
        assert!(out.contains(r#"<pivotField axis="axisRow" showAll="0"/>"#));
        assert!(out.contains(r#"<pivotField dataField="1" showAll="0"/>"#));
        // Stale cached layout dropped; everything else preserved.
        assert!(!out.contains("rowItems"));
        assert!(out.contains(r#"<location ref="A3:B6""#));
        assert!(out.contains("PivotStyleLight16"));
        // The rewritten definition parses back to the edited model.
        let (p2, _) = parse_pivot_table_xml(&out, 0, "p").unwrap();
        assert_eq!(p2.row_fields, vec![1]);
        assert_eq!(p2.col_fields, vec![0]);
        assert_eq!(p2.data_fields[0].agg, Agg::Average);
        assert!(!p2.unsupported);
        // Two measures put the Values pseudo-field on the column axis.
        piv.data_fields.push(DataField {
            name: "Count of Sales".into(),
            field: 2,
            agg: Agg::Count,
        });
        let out = rewrite_pivot_definition(xml, &piv);
        assert!(out.contains(r#"<colFields count="2"><field x="0"/><field x="-2"/></colFields>"#));
    }

    #[test]
    fn subtotals_follow_default_subtotal_attrs() {
        let two_rows = |fields: &str| {
            format!(
                r#"<pivotTableDefinition name="P" cacheId="1"><location ref="A1:B2"/><pivotFields>{fields}</pivotFields><rowFields><field x="0"/><field x="1"/></rowFields><dataFields><dataField fld="2"/></dataFields></pivotTableDefinition>"#
            )
        };
        // Two nested row fields, defaults on → subtotals.
        let (p, _) = parse_pivot_table_xml(
            &two_rows(r#"<pivotField axis="axisRow"/><pivotField axis="axisRow"/><pivotField dataField="1"/>"#),
            0,
            "p",
        )
        .unwrap();
        assert!(p.subtotals);
        // Every row field opted out → no subtotals.
        let (p, _) = parse_pivot_table_xml(
            &two_rows(r#"<pivotField axis="axisRow" defaultSubtotal="0"/><pivotField axis="axisRow" defaultSubtotal="0"/><pivotField dataField="1"/>"#),
            0,
            "p",
        )
        .unwrap();
        assert!(!p.subtotals);
        // A single row field never shows subtotals (grand total covers it).
        let one_row = r#"<pivotTableDefinition name="P" cacheId="1"><location ref="A1:B2"/><pivotFields><pivotField axis="axisRow"/><pivotField dataField="1"/></pivotFields><rowFields><field x="0"/></rowFields><dataFields><dataField fld="1"/></dataFields></pivotTableDefinition>"#;
        let (p, _) = parse_pivot_table_xml(one_row, 0, "p").unwrap();
        assert!(!p.subtotals);
    }

    #[test]
    fn cache_parses_table_and_range_sources() {
        let range = r#"<pivotCacheDefinition><cacheSource type="worksheet"><worksheetSource ref="A1:C5" sheet="Data"/></cacheSource><cacheFields><cacheField name="A"/><cacheField name="B"/></cacheFields></pivotCacheDefinition>"#;
        let (src, fields, _, _, unsupported) = parse_pivot_cache_xml(range, false).unwrap();
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
        let (src, _, _, _, _) = parse_pivot_cache_xml(table, false).unwrap();
        assert_eq!(src, PivotSource::Table("Sales".into()));
        // A calculated field is captured (its formula) rather than poisoning refresh.
        let calc = r#"<pivotCacheDefinition><cacheSource type="worksheet"><worksheetSource ref="A1:B2" sheet="D"/></cacheSource><cacheFields><cacheField name="A"/><cacheField name="C" formula="A*2"/></cacheFields></pivotCacheDefinition>"#;
        let (_, _, _, calc_formulas, unsupported) = parse_pivot_cache_xml(calc, false).unwrap();
        assert!(!unsupported);
        assert_eq!(calc_formulas, vec![(1, "A*2".to_string())]);
    }

    #[test]
    fn refresh_applies_hidden_and_page_filters() {
        use crate::sheet::{Cell, CellValue, Sheet};
        let make_data = || {
            let mut data = Sheet {
                name: "Data".into(),
                ..Sheet::default()
            };
            data.set_cell(0, 0, Cell::text("Region"));
            data.set_cell(0, 1, Cell::text("Amount"));
            for (i, (reg, amt)) in [("East", 10.0), ("West", 20.0), ("East", 30.0), ("West", 40.0)]
                .iter()
                .enumerate()
            {
                data.set_cell(i as u32 + 1, 0, Cell::text(reg));
                data.set_cell(i as u32 + 1, 1, Cell::number(*amt));
            }
            data
        };
        let base_pivot = |hidden: Vec<(usize, Vec<usize>)>, page: Vec<(usize, Option<usize>)>| Pivot {
            name: "P".into(),
            sheet: 1,
            location: (0, 0, 2, 1),
            source: PivotSource::Range {
                sheet: "Data".into(),
                rect: (0, 0, 4, 1),
            },
            fields: vec!["Region".into(), "Amount".into()],
            row_fields: vec![0],
            col_fields: vec![],
            data_fields: vec![DataField {
                name: "Sum of Amount".into(),
                field: 1,
                agg: Agg::Sum,
            }],
            field_items: vec![vec![Value::Str("East".into()), Value::Str("West".into())], vec![]],
            hidden,
            page,
            items_order: vec![],
            calc_formulas: vec![],
            grand_rows: true,
            grand_cols: true,
            subtotals: false,
            unsupported: false,
            edited: false,
            part: String::new(),
            cache_part: String::new(),
        };
        let nums = |wb: &Workbook| -> Vec<f64> {
            (0..3)
                .flat_map(|r| (0..2).map(move |c| (r, c)))
                .filter_map(|(r, c)| match wb.sheets[1].cell(r, c).map(|cl| cl.value.clone()) {
                    Some(CellValue::Number(n)) => Some(n),
                    _ => None,
                })
                .collect()
        };

        // Hide "West" (shared index 1): only East's 10+30 = 40 aggregates.
        let mut wb = Workbook {
            sheets: vec![make_data(), Sheet::default()],
            ..Workbook::default()
        };
        wb.pivots.push(base_pivot(vec![(0, vec![1])], vec![]));
        assert_eq!(refresh_pivots(&mut wb).refreshed, 1);
        let v = nums(&wb);
        assert!(v.contains(&40.0), "East 40 missing: {v:?}");
        assert!(!v.contains(&20.0) && !v.contains(&60.0), "West leaked: {v:?}");

        // A page filter selecting item 0 ("East") gives the same result.
        let mut wb = Workbook {
            sheets: vec![make_data(), Sheet::default()],
            ..Workbook::default()
        };
        wb.pivots.push(base_pivot(vec![], vec![(0, Some(0))]));
        assert_eq!(refresh_pivots(&mut wb).refreshed, 1);
        let v = nums(&wb);
        assert!(v.contains(&40.0), "page-filtered East 40 missing: {v:?}");
        assert!(!v.contains(&60.0), "page filter leaked West: {v:?}");
    }

    #[test]
    fn refresh_evaluates_calculated_field() {
        use crate::sheet::{Cell, CellValue, Sheet};
        let mut data = Sheet {
            name: "Data".into(),
            ..Sheet::default()
        };
        for (c, h) in ["Region", "Sales", "Cost"].iter().enumerate() {
            data.set_cell(0, c as u32, Cell::text(h));
        }
        for (i, (reg, s, c)) in [("East", 100.0, 60.0), ("West", 50.0, 20.0), ("East", 30.0, 10.0)]
            .iter()
            .enumerate()
        {
            data.set_cell(i as u32 + 1, 0, Cell::text(reg));
            data.set_cell(i as u32 + 1, 1, Cell::number(*s));
            data.set_cell(i as u32 + 1, 2, Cell::number(*c));
        }
        let mut wb = Workbook {
            sheets: vec![data, Sheet::default()],
            ..Workbook::default()
        };
        let mut piv = new_pivot();
        piv.sheet = 1;
        piv.location = (0, 0, 2, 1);
        piv.source = PivotSource::Range {
            sheet: "Data".into(),
            rect: (0, 0, 3, 2),
        };
        // "Margin" (field 3) is a calculated field = Sales - Cost.
        piv.fields = vec!["Region".into(), "Sales".into(), "Cost".into(), "Margin".into()];
        piv.calc_formulas = vec![(3, "Sales - Cost".into())];
        piv.row_fields = vec![0];
        piv.data_fields = vec![DataField {
            name: "Margin".into(),
            field: 3,
            agg: Agg::Sum,
        }];
        wb.pivots.push(piv);
        assert_eq!(refresh_pivots(&mut wb).refreshed, 1);
        // East Margin = 130 - 70 = 60; West = 50 - 20 = 30; Grand = 180 - 90 = 90.
        let nums: Vec<f64> = (0..5)
            .flat_map(|r| (0..2).map(move |c| (r, c)))
            .filter_map(|(r, c)| match wb.sheets[1].cell(r, c).map(|cl| cl.value.clone()) {
                Some(CellValue::Number(n)) => Some(n),
                _ => None,
            })
            .collect();
        assert!(nums.contains(&60.0), "East margin 60 missing: {nums:?}");
        assert!(nums.contains(&30.0), "West margin 30 missing: {nums:?}");
        assert!(nums.contains(&90.0), "grand margin 90 missing: {nums:?}");
    }

    /// A minimal supported pivot for tests (fill in what each needs).
    fn new_pivot() -> Pivot {
        Pivot {
            name: "P".into(),
            sheet: 0,
            location: (0, 0, 0, 0),
            source: PivotSource::Table(String::new()),
            fields: Vec::new(),
            row_fields: Vec::new(),
            col_fields: Vec::new(),
            data_fields: Vec::new(),
            field_items: Vec::new(),
            hidden: Vec::new(),
            page: Vec::new(),
            items_order: Vec::new(),
            calc_formulas: Vec::new(),
            grand_rows: true,
            grand_cols: true,
            subtotals: false,
            unsupported: false,
            edited: false,
            part: String::new(),
            cache_part: String::new(),
        }
    }

    #[test]
    fn cache_parses_shared_items() {
        let xml = r#"<pivotCacheDefinition><cacheSource type="worksheet"><worksheetSource ref="A1:C5" sheet="D"/></cacheSource><cacheFields>
            <cacheField name="Region"><sharedItems><s v="East"/><s v="West"/></sharedItems></cacheField>
            <cacheField name="Flag"><sharedItems count="2"><b v="1"/><b v="0"/></sharedItems></cacheField>
            <cacheField name="Amount"><sharedItems containsNumber="1" minValue="1" maxValue="9"/></cacheField>
            </cacheFields></pivotCacheDefinition>"#;
        let (_, fields, items, _, _) = parse_pivot_cache_xml(xml, false).unwrap();
        assert_eq!(fields, vec!["Region", "Flag", "Amount"]);
        assert_eq!(items[0], vec![Value::Str("East".into()), Value::Str("West".into())]);
        assert_eq!(items[1], vec![Value::Bool(true), Value::Bool(false)]);
        assert!(items[2].is_empty()); // numeric range → no enumerated items
    }
}
