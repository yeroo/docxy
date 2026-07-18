//! Columnar query core: snapshot a rectangular region into named columns,
//! then filter / group / aggregate it.
//!
//! This layer is deliberately independent of the xlsx file format — pivot
//! refresh sits on top of it, and the later data-model phases (multiple
//! tables, relationships, measures) are meant to grow here, not in the XML
//! plumbing.

use crate::engine::cell_to_value;
use crate::formula::{ExcelError, Value, compare, to_text};
use crate::sheet::Workbook;

/// A columnar snapshot: equal-length value columns with header names.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub names: Vec<String>,
    pub cols: Vec<Vec<Value>>,
}

impl Frame {
    /// Snapshot a rect whose first row is the header row.
    pub fn from_range(wb: &Workbook, sheet: usize, rect: (u32, u32, u32, u32)) -> Frame {
        let (r1, c1, r2, c2) = rect;
        let mut names = Vec::new();
        let mut cols = Vec::new();
        let Some(sh) = wb.sheets.get(sheet) else {
            return Frame::default();
        };
        for c in c1..=c2 {
            let head = sh
                .cell(r1, c)
                .map(|cl| cell_to_value(&cl.value))
                .unwrap_or(Value::Empty);
            let name = match to_text(&head) {
                Ok(t) if !t.is_empty() => t,
                _ => format!("Column{}", c - c1 + 1),
            };
            names.push(name);
            let mut col = Vec::with_capacity((r2 - r1) as usize);
            for r in (r1 + 1)..=r2 {
                col.push(
                    sh.cell(r, c)
                        .map(|cl| cell_to_value(&cl.value))
                        .unwrap_or(Value::Empty),
                );
            }
            cols.push(col);
        }
        Frame { names, cols }
    }

    /// Snapshot an Excel Table's data region, headers from its column names.
    pub fn from_table(wb: &Workbook, name: &str) -> Option<Frame> {
        let t = wb.table(name)?;
        let (r1, c1, r2, c2) = t.range;
        let data_r1 = r1 + t.header_rows;
        let data_r2 = r2.checked_sub(t.totals_rows)?;
        let sh = wb.sheets.get(t.sheet)?;
        let mut cols = Vec::new();
        for c in c1..=c2 {
            let mut col = Vec::new();
            if data_r1 <= data_r2 {
                for r in data_r1..=data_r2 {
                    col.push(
                        sh.cell(r, c)
                            .map(|cl| cell_to_value(&cl.value))
                            .unwrap_or(Value::Empty),
                    );
                }
            }
            cols.push(col);
        }
        Some(Frame {
            names: t.columns.clone(),
            cols,
        })
    }

    pub fn rows(&self) -> usize {
        self.cols.first().map(|c| c.len()).unwrap_or(0)
    }

    /// Column index by header name, case-insensitive.
    pub fn col_index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(name))
    }

    /// Parse delimited text (RFC 4180-ish: quoted fields, doubled quotes,
    /// CRLF) into a Frame, sniffing the delimiter — comma, semicolon, or tab
    /// — from the first record. The first record is the header row; fields
    /// are typed by inference (number, TRUE/FALSE, empty, text). Ragged
    /// records are padded with empties.
    pub fn from_csv(text: &str) -> Frame {
        let records = parse_delimited(text, sniff_delimiter(text));
        let mut it = records.into_iter();
        let Some(header) = it.next() else {
            return Frame::default();
        };
        let names: Vec<String> = header
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let t = h.trim();
                if t.is_empty() {
                    format!("Column{}", i + 1)
                } else {
                    t.to_string()
                }
            })
            .collect();
        let width = names.len().max(1);
        let mut cols: Vec<Vec<Value>> = vec![Vec::new(); width];
        for rec in it {
            for (i, col) in cols.iter_mut().enumerate() {
                col.push(infer_value(rec.get(i).map(String::as_str).unwrap_or("")));
            }
        }
        Frame { names, cols }
    }
}

/// CSV field → typed value.
fn infer_value(field: &str) -> Value {
    let t = field.trim();
    if t.is_empty() {
        return Value::Empty;
    }
    if let Ok(n) = t.parse::<f64>() {
        if n.is_finite() {
            return Value::Num(n);
        }
    }
    if t.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if t.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    Value::Str(field.to_string())
}

/// Pick the likeliest delimiter from the first record: the candidate
/// (comma, semicolon, tab) occurring most often outside quotes.
pub fn sniff_delimiter(text: &str) -> char {
    let mut counts = [0usize; 3]; // , ; \t
    let mut in_quotes = false;
    for ch in text.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '\n' if !in_quotes => break,
            ',' if !in_quotes => counts[0] += 1,
            ';' if !in_quotes => counts[1] += 1,
            '\t' if !in_quotes => counts[2] += 1,
            _ => {}
        }
    }
    let best = (0..3).max_by_key(|&i| counts[i]).unwrap_or(0);
    if counts[best] == 0 {
        ','
    } else {
        [',', ';', '\t'][best]
    }
}

/// Split CSV text into records of raw string fields.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    parse_delimited(text, ',')
}

/// Split delimiter-separated text into records of raw string fields.
pub fn parse_delimited(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(ch) = chars.next() {
        any = true;
        if in_quotes {
            match ch {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                _ => field.push(ch),
            }
        } else {
            match ch {
                '\"' => in_quotes = true,
                c if c == delim => {
                    record.push(std::mem::take(&mut field));
                    any = true;
                }
                '\r' => {} // swallowed; \n terminates the record
                '\n' => {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                }
                _ => field.push(ch),
            }
        }
    }
    if any && (!field.is_empty() || !record.is_empty()) {
        record.push(field);
        records.push(record);
    }
    records
}

/// Pivot aggregation functions (the `subtotal` values of SpreadsheetML data
/// fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agg {
    Sum,
    Count,
    CountNums,
    Average,
    Max,
    Min,
    Product,
    StdDev,
    StdDevP,
    Var,
    VarP,
}

impl Agg {
    /// From the `dataField/@subtotal` attribute (absent = sum).
    pub fn from_subtotal(s: &str) -> Option<Agg> {
        Some(match s {
            "" | "sum" => Agg::Sum,
            "count" => Agg::Count,
            "countNums" => Agg::CountNums,
            "average" => Agg::Average,
            "max" => Agg::Max,
            "min" => Agg::Min,
            "product" => Agg::Product,
            "stdDev" => Agg::StdDev,
            "stdDevp" | "stdDevP" => Agg::StdDevP,
            "var" => Agg::Var,
            "varp" | "varP" => Agg::VarP,
            _ => return None,
        })
    }

    /// From the wire-contract agg strings used by `sheet.pivot` and its wasm
    /// mirror (the spec's 11 values): `sum, count, countNums, average, max,
    /// min, product, stdDev, stdDevP, var, varP`. Distinct spellings from
    /// [`Agg::from_subtotal`]'s OOXML attribute values (`stdDevp`/`varp`,
    /// lowercase `p`) — this is the wire contract, not the file format.
    pub fn from_verb_name(s: &str) -> Option<Agg> {
        Some(match s {
            "sum" => Agg::Sum,
            "count" => Agg::Count,
            "countNums" => Agg::CountNums,
            "average" => Agg::Average,
            "max" => Agg::Max,
            "min" => Agg::Min,
            "product" => Agg::Product,
            "stdDev" => Agg::StdDev,
            "stdDevP" => Agg::StdDevP,
            "var" => Agg::Var,
            "varP" => Agg::VarP,
            _ => return None,
        })
    }

    /// The `subtotal` attribute value to store; `None` = sum (omitted).
    pub fn subtotal_code(&self) -> Option<&'static str> {
        Some(match self {
            Agg::Sum => return None,
            Agg::Count => "count",
            Agg::CountNums => "countNums",
            Agg::Average => "average",
            Agg::Max => "max",
            Agg::Min => "min",
            Agg::Product => "product",
            Agg::StdDev => "stdDev",
            Agg::StdDevP => "stdDevp",
            Agg::Var => "var",
            Agg::VarP => "varp",
        })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Agg::Sum => "Sum",
            Agg::Count => "Count",
            Agg::CountNums => "Count Nums",
            Agg::Average => "Average",
            Agg::Max => "Max",
            Agg::Min => "Min",
            Agg::Product => "Product",
            Agg::StdDev => "StdDev",
            Agg::StdDevP => "StdDevp",
            Agg::Var => "Var",
            Agg::VarP => "Varp",
        }
    }

    /// Aggregate one bucket. `None` values never reach here — buckets hold
    /// the actual cell values of contributing records. An empty bucket (no
    /// records) renders blank, which the caller encodes as `Value::Empty`.
    pub fn apply(&self, vals: &[Value]) -> Value {
        if vals.is_empty() {
            return Value::Empty;
        }
        let nums: Vec<f64> = vals
            .iter()
            .filter_map(|v| match v {
                Value::Num(n) => Some(*n),
                _ => None,
            })
            .collect();
        let n = nums.len() as f64;
        match self {
            Agg::Count => {
                Value::Num(vals.iter().filter(|v| !matches!(v, Value::Empty)).count() as f64)
            }
            Agg::CountNums => Value::Num(n),
            Agg::Sum => Value::Num(nums.iter().sum()),
            Agg::Product => Value::Num(nums.iter().product()),
            Agg::Max => {
                Value::Num(nums.iter().copied().fold(f64::MIN, f64::max)).pipe_fix_empty(&nums)
            }
            Agg::Min => {
                Value::Num(nums.iter().copied().fold(f64::MAX, f64::min)).pipe_fix_empty(&nums)
            }
            Agg::Average => {
                if nums.is_empty() {
                    Value::Err(ExcelError::Div0)
                } else {
                    Value::Num(nums.iter().sum::<f64>() / n)
                }
            }
            Agg::StdDev | Agg::StdDevP | Agg::Var | Agg::VarP => {
                let pop = matches!(self, Agg::StdDevP | Agg::VarP);
                let denom = if pop { n } else { n - 1.0 };
                if denom <= 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                let mean = nums.iter().sum::<f64>() / n;
                let var = nums.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / denom;
                if matches!(self, Agg::StdDev | Agg::StdDevP) {
                    Value::Num(var.sqrt())
                } else {
                    Value::Num(var)
                }
            }
        }
    }
}

trait PipeFixEmpty {
    fn pipe_fix_empty(self, nums: &[f64]) -> Value;
}
impl PipeFixEmpty for Value {
    /// Max/Min over a bucket with records but no numbers shows 0 in Excel.
    fn pipe_fix_empty(self, nums: &[f64]) -> Value {
        if nums.is_empty() {
            Value::Num(0.0)
        } else {
            self
        }
    }
}

/// One value column of a pivot: which frame column, how to aggregate it,
/// and the display name ("Sum of Sales").
#[derive(Clone, Debug)]
pub struct Measure {
    pub col: usize,
    pub agg: Agg,
    pub name: String,
    /// A calculated field: when set, the cell value is this expression evaluated
    /// over the group's per-field sums (ignoring `col`/`agg`).
    pub calc: Option<crate::pivotcalc::CalcExpr>,
}

/// A pivot query over a [`Frame`].
#[derive(Clone, Debug, Default)]
pub struct PivotSpec {
    /// Frame columns grouped on rows (outer to inner).
    pub rows: Vec<usize>,
    /// Frame columns grouped across columns (outer to inner).
    pub cols: Vec<usize>,
    pub measures: Vec<Measure>,
    /// Keep only records whose column value is in the set (case-insensitive
    /// for text). Empty spec = no filtering.
    pub filters: Vec<(usize, Vec<Value>)>,
    /// Grand-total row / column.
    pub grand_rows: bool,
    pub grand_cols: bool,
    /// Subtotal rows after each outer row-field group ("East Total"),
    /// Excel's tabular-with-subtotals layout. Only meaningful with two or
    /// more row fields.
    pub subtotals: bool,
    /// Excel's "values on rows" (`dataOnRows`): stack the measures down the row
    /// axis (each row group gets one sub-row per measure) rather than across
    /// columns. Handled for the common case of no column fields.
    pub data_on_rows: bool,
}

/// A computed pivot, laid out as a rectangular grid ready to render:
/// `header_rows` rows of column headers on top, `label_cols` columns of row
/// labels on the left, aggregates in the body.
#[derive(Clone, Debug)]
pub struct PivotOut {
    pub grid: Vec<Vec<Value>>,
    pub header_rows: usize,
    pub label_cols: usize,
}

/// A grouping key: normalized for equality/hashing, displaying the first
/// value seen (Excel groups text case-insensitively).
pub(crate) fn key_of(v: &Value) -> String {
    match v {
        Value::Empty => "\u{0}empty".to_string(),
        Value::Num(n) => format!("\u{0}n{}", n.to_bits()),
        Value::Str(s) => format!("\u{0}s{}", s.to_lowercase()),
        Value::Bool(b) => format!("\u{0}b{b}"),
        Value::Err(e) => format!("\u{0}e{}", e.code()),
    }
}

pub(crate) fn keys_lt(a: &[Value], b: &[Value]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b) {
        let ord = compare(x, y).unwrap_or(std::cmp::Ordering::Equal);
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

/// Run a pivot query. Group combos appear sorted ascending (numbers, then
/// text case-insensitively, then logicals — Excel's default sort).
pub fn pivot(f: &Frame, spec: &PivotSpec) -> PivotOut {
    let nrows = f.rows();
    // 1. Filter.
    let keep: Vec<usize> = (0..nrows)
        .filter(|&r| {
            spec.filters.iter().all(|(c, allowed)| {
                let v = &f.cols[*c][r];
                allowed
                    .iter()
                    .any(|a| compare(a, v) == Ok(std::cmp::Ordering::Equal))
            })
        })
        .collect();

    // 2. Distinct sorted key combos along each axis. Display values are
    // canonicalized per field to the first occurrence ("east" after "East"
    // groups into — and shows as — "East").
    let canon = |c: usize, r: usize| -> Value {
        let key = key_of(&f.cols[c][r]);
        for &rr in &keep {
            if key_of(&f.cols[c][rr]) == key {
                return f.cols[c][rr].clone();
            }
        }
        f.cols[c][r].clone()
    };
    let combos = |fields: &[usize]| -> Vec<Vec<Value>> {
        if fields.is_empty() {
            return vec![Vec::new()];
        }
        let mut seen: Vec<(String, Vec<Value>)> = Vec::new();
        for &r in &keep {
            let disp: Vec<Value> = fields.iter().map(|&c| canon(c, r)).collect();
            let key: String = disp.iter().map(key_of).collect();
            if !seen.iter().any(|(k, _)| *k == key) {
                seen.push((key, disp));
            }
        }
        let mut out: Vec<Vec<Value>> = seen.into_iter().map(|(_, d)| d).collect();
        out.sort_by(|a, b| keys_lt(a, b));
        out
    };
    let row_combos = combos(&spec.rows);
    let col_combos = combos(&spec.cols);

    // 3. Bucket records by (row combo, col combo).
    let combo_index = |fields: &[usize], combos: &Vec<Vec<Value>>, r: usize| -> usize {
        let key: String = fields.iter().map(|&c| key_of(&f.cols[c][r])).collect();
        combos
            .iter()
            .position(|combo| combo.iter().map(key_of).collect::<String>() == key)
            .unwrap_or(0)
    };
    let mut buckets: Vec<Vec<Vec<usize>>> =
        vec![vec![Vec::new(); col_combos.len()]; row_combos.len()];
    for &r in &keep {
        let ri = combo_index(&spec.rows, &row_combos, r);
        let ci = combo_index(&spec.cols, &col_combos, r);
        buckets[ri][ci].push(r);
    }

    // 4. Lay out the grid.
    let m = spec.measures.len().max(1);
    let label_cols = spec.rows.len().max(1);
    let header_rows = spec.cols.len() + 1;
    let grand_col = spec.grand_cols && !spec.cols.is_empty();
    let data_cols = col_combos.len() * m + if grand_col { m } else { 0 };
    let total_cols = label_cols + data_cols;

    // Group sum of a base column (numbers + booleans), for calculated fields.
    let sum_col = |records: &[usize], col: usize| -> f64 {
        records
            .iter()
            .map(|&r| match &f.cols[col][r] {
                Value::Num(n) => *n,
                Value::Bool(b) => *b as u8 as f64,
                _ => 0.0,
            })
            .sum()
    };
    let agg_records = |records: &[usize], meas: &Measure| -> Value {
        if let Some(calc) = &meas.calc {
            return crate::pivotcalc::eval(calc, &|col| sum_col(records, col));
        }
        let vals: Vec<Value> = records
            .iter()
            .map(|&r| f.cols[meas.col][r].clone())
            .collect();
        meas.agg.apply(&vals)
    };

    // "Values on rows" with more than one measure and no column fields: stack the
    // measures down the rows, with the measure name in an extra inner label column.
    if spec.data_on_rows && spec.measures.len() > 1 && spec.cols.is_empty() {
        let label_cols = spec.rows.len().max(1) + 1;
        let mname_col = label_cols - 1;
        let val_col = label_cols;
        let total_cols = label_cols + 1;
        let mut grid: Vec<Vec<Value>> = Vec::new();
        // Header: row-field names in the corner, "Total" over the value column.
        let mut hdr = vec![Value::Empty; total_cols];
        for (i, &rf) in spec.rows.iter().enumerate() {
            hdr[i] = Value::Str(f.names[rf].clone());
        }
        hdr[val_col] = Value::Str("Total".into());
        grid.push(hdr);
        // Each row group → one sub-row per measure (labels only on the first).
        for (ri, combo) in row_combos.iter().enumerate() {
            let recs: Vec<usize> = buckets[ri].iter().flatten().copied().collect();
            for (k, meas) in spec.measures.iter().enumerate() {
                let mut row = vec![Value::Empty; total_cols];
                if k == 0 {
                    if combo.is_empty() {
                        row[0] = Value::Str("Total".into());
                    }
                    for (i, v) in combo.iter().enumerate() {
                        row[i] = v.clone();
                    }
                }
                row[mname_col] = Value::Str(meas.name.clone());
                row[val_col] = agg_records(&recs, meas);
                grid.push(row);
            }
        }
        if spec.grand_rows && !row_combos.is_empty() && !spec.rows.is_empty() {
            let all: Vec<usize> = buckets.iter().flatten().flatten().copied().collect();
            for (k, meas) in spec.measures.iter().enumerate() {
                let mut row = vec![Value::Empty; total_cols];
                if k == 0 {
                    row[0] = Value::Str("Grand Total".into());
                }
                row[mname_col] = Value::Str(meas.name.clone());
                row[val_col] = agg_records(&all, meas);
                grid.push(row);
            }
        }
        return PivotOut {
            grid,
            header_rows: 1,
            label_cols,
        };
    }

    let mut grid: Vec<Vec<Value>> = Vec::new();
    // Column-field header rows.
    for h in 0..spec.cols.len() {
        let mut row = vec![Value::Empty; total_cols];
        for (ci, combo) in col_combos.iter().enumerate() {
            for k in 0..m {
                row[label_cols + ci * m + k] = combo[h].clone();
            }
        }
        if grand_col && h == 0 {
            for k in 0..m {
                row[label_cols + col_combos.len() * m + k] = Value::Str("Grand Total".into());
            }
        }
        grid.push(row);
    }
    // Measure-name header row; row-field names in the corner.
    {
        let mut row = vec![Value::Empty; total_cols];
        for (i, &rf) in spec.rows.iter().enumerate() {
            row[i] = Value::Str(f.names[rf].clone());
        }
        for ci in 0..col_combos.len() + usize::from(grand_col) {
            for (k, meas) in spec.measures.iter().enumerate() {
                row[label_cols + ci * m + k] = Value::Str(meas.name.clone());
            }
        }
        grid.push(row);
    }
    // Data rows, with control-break subtotals for the non-innermost row
    // fields when asked: after each level-l group, a "<value> Total" row
    // aggregating the whole group (deepest level closes first).
    let sub_levels = if spec.subtotals && spec.rows.len() >= 2 {
        spec.rows.len() - 1
    } else {
        0
    };
    // group_start[l] = index of the first combo of the current level-l group.
    let mut group_start = vec![0usize; sub_levels];
    let emit_subtotal = |grid: &mut Vec<Vec<Value>>,
                         buckets: &Vec<Vec<Vec<usize>>>,
                         level: usize,
                         from: usize,
                         to: usize,
                         label_of: &Value| {
        let mut row = vec![Value::Empty; total_cols];
        let label = to_text(label_of).unwrap_or_default();
        row[level] = Value::Str(format!("{label} Total"));
        for ci in 0..col_combos.len() {
            let group: Vec<usize> = (from..to).flat_map(|rj| buckets[rj][ci].clone()).collect();
            for (k, meas) in spec.measures.iter().enumerate() {
                row[label_cols + ci * m + k] = agg_records(&group, meas);
            }
        }
        if grand_col {
            let all: Vec<usize> = (from..to)
                .flat_map(|rj| buckets[rj].iter().flatten().copied().collect::<Vec<_>>())
                .collect();
            for (k, meas) in spec.measures.iter().enumerate() {
                row[label_cols + col_combos.len() * m + k] = agg_records(&all, meas);
            }
        }
        grid.push(row);
    };
    for (ri, combo) in row_combos.iter().enumerate() {
        // Close finished groups (deepest first) before starting a new one.
        if ri > 0 {
            for level in (0..sub_levels).rev() {
                let changed =
                    (0..=level).any(|l| key_of(&row_combos[ri - 1][l]) != key_of(&combo[l]));
                if changed {
                    emit_subtotal(
                        &mut grid,
                        &buckets,
                        level,
                        group_start[level],
                        ri,
                        &row_combos[ri - 1][level],
                    );
                    group_start[level] = ri;
                }
            }
        }
        let mut row = vec![Value::Empty; total_cols];
        if combo.is_empty() {
            row[0] = Value::Str("Total".into());
        }
        for (i, v) in combo.iter().enumerate() {
            row[i] = v.clone();
        }
        for ci in 0..col_combos.len() {
            for (k, meas) in spec.measures.iter().enumerate() {
                row[label_cols + ci * m + k] = agg_records(&buckets[ri][ci], meas);
            }
        }
        if grand_col {
            let all: Vec<usize> = buckets[ri].iter().flatten().copied().collect();
            for (k, meas) in spec.measures.iter().enumerate() {
                row[label_cols + col_combos.len() * m + k] = agg_records(&all, meas);
            }
        }
        grid.push(row);
    }
    if !row_combos.is_empty() {
        for level in (0..sub_levels).rev() {
            emit_subtotal(
                &mut grid,
                &buckets,
                level,
                group_start[level],
                row_combos.len(),
                &row_combos[row_combos.len() - 1][level],
            );
        }
    }
    // Grand-total row.
    if spec.grand_rows && !row_combos.is_empty() && !spec.rows.is_empty() {
        let mut row = vec![Value::Empty; total_cols];
        row[0] = Value::Str("Grand Total".into());
        for ci in 0..col_combos.len() {
            let col_all: Vec<usize> = buckets.iter().flat_map(|b| b[ci].iter().copied()).collect();
            for (k, meas) in spec.measures.iter().enumerate() {
                row[label_cols + ci * m + k] = agg_records(&col_all, meas);
            }
        }
        if grand_col {
            let all: Vec<usize> = buckets.iter().flatten().flatten().copied().collect();
            for (k, meas) in spec.measures.iter().enumerate() {
                row[label_cols + col_combos.len() * m + k] = agg_records(&all, meas);
            }
        }
        grid.push(row);
    }

    PivotOut {
        grid,
        header_rows,
        label_cols,
    }
}

/// Build a [`PivotSpec`] from column *header names* — the wire contract
/// shared by `sheet.pivot` (xlsxy) and its wasm mirror — resolving each name
/// against `frame` via [`Frame::col_index`]. Value measures get an
/// Excel-style display name ("Sum of Sales"). On the first name that doesn't
/// match any header, returns that name as `Err` so the caller can report
/// which column was unknown.
pub fn pivot_spec_from_names(
    frame: &Frame,
    rows: &[String],
    cols: &[String],
    values: &[(String, Agg)],
) -> Result<PivotSpec, String> {
    let resolve = |name: &String| frame.col_index(name).ok_or_else(|| name.clone());
    let rows = rows.iter().map(resolve).collect::<Result<Vec<_>, _>>()?;
    let cols = cols.iter().map(resolve).collect::<Result<Vec<_>, _>>()?;
    let measures = values
        .iter()
        .map(|(name, agg)| {
            let idx = resolve(name)?;
            Ok(Measure {
                col: idx,
                agg: *agg,
                name: format!("{} of {}", agg.label(), frame.names[idx]),
                calc: None,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(PivotSpec {
        rows,
        cols,
        measures,
        ..PivotSpec::default()
    })
}

/// The reply shape for `sheet.pivot`'s `table` field: a [`PivotOut`]'s
/// computed grid (header rows and label columns included, as `pivot` laid
/// them out) rendered as display strings — General-formatted numbers, error
/// codes (`#DIV/0!`) for error cells, empty string for blank cells.
pub fn pivot_table_strings(out: &PivotOut) -> Vec<Vec<String>> {
    out.grid
        .iter()
        .map(|row| {
            row.iter()
                .map(|v| match v {
                    Value::Err(e) => e.code().to_string(),
                    other => to_text(other).unwrap_or_default(),
                })
                .collect()
        })
        .collect()
}

/// Summary statistics for `sheet.stats`: mirrors the TUI's selection-stats
/// status line (`count` = non-blank cells; `sum`/`average`/`min`/`max`
/// computed over numeric cells only), plus `count_nums` (the numeric subset
/// of `count`). All four numeric fields are `0.0` over a range with no
/// numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RangeStats {
    pub sum: f64,
    pub count: usize,
    pub count_nums: usize,
    pub average: f64,
    pub min: f64,
    pub max: f64,
}

/// Compute [`RangeStats`] over an arbitrary set of cell values (typically one
/// `sheet.stats` range, in any order).
pub fn range_stats(vals: &[Value]) -> RangeStats {
    let mut count = 0usize;
    let mut nums: Vec<f64> = Vec::new();
    for v in vals {
        if matches!(v, Value::Empty) {
            continue;
        }
        count += 1;
        if let Value::Num(n) = v {
            nums.push(*n);
        }
    }
    let count_nums = nums.len();
    let sum: f64 = nums.iter().sum();
    let (average, min, max) = if count_nums > 0 {
        (
            sum / count_nums as f64,
            nums.iter().copied().fold(f64::INFINITY, f64::min),
            nums.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        )
    } else {
        (0.0, 0.0, 0.0)
    };
    RangeStats {
        sum,
        count,
        count_nums,
        average,
        min,
        max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{Cell, Sheet};

    /// Region | Product | Qty | Sales
    fn sales_wb() -> Workbook {
        let rows: Vec<(&str, &str, f64, f64)> = vec![
            ("East", "Pen", 3.0, 30.0),
            ("West", "Pad", 2.0, 40.0),
            ("East", "Pad", 1.0, 20.0),
            ("West", "Pen", 5.0, 50.0),
            ("East", "Pen", 2.0, 20.0),
            ("east", "Ink", 4.0, 10.0), // case-insensitive grouping with East
        ];
        let mut sh = Sheet {
            name: "Data".to_string(),
            ..Sheet::default()
        };
        for (c, h) in ["Region", "Product", "Qty", "Sales"].iter().enumerate() {
            sh.set_cell(0, c as u32, Cell::text(h));
        }
        for (i, (reg, prod, qty, sales)) in rows.iter().enumerate() {
            let r = i as u32 + 1;
            sh.set_cell(r, 0, Cell::text(reg));
            sh.set_cell(r, 1, Cell::text(prod));
            sh.set_cell(r, 2, Cell::number(*qty));
            sh.set_cell(r, 3, Cell::number(*sales));
        }
        Workbook {
            sheets: vec![sh],
            ..Workbook::default()
        }
    }

    fn s(v: &str) -> Value {
        Value::Str(v.to_string())
    }
    fn n(x: f64) -> Value {
        Value::Num(x)
    }

    #[test]
    fn frame_snapshot() {
        let wb = sales_wb();
        let f = Frame::from_range(&wb, 0, (0, 0, 6, 3));
        assert_eq!(f.names, vec!["Region", "Product", "Qty", "Sales"]);
        assert_eq!(f.rows(), 6);
        assert_eq!(f.col_index("sales"), Some(3));
        assert_eq!(f.cols[3][1], n(40.0));
    }

    #[test]
    fn single_row_field_sum() {
        let wb = sales_wb();
        let f = Frame::from_range(&wb, 0, (0, 0, 6, 3));
        let spec = PivotSpec {
            rows: vec![0],
            measures: vec![Measure {
                col: 3,
                agg: Agg::Sum,
                name: "Sum of Sales".into(),
                calc: None,
            }],
            grand_rows: true,
            ..PivotSpec::default()
        };
        let out = pivot(&f, &spec);
        // Header, East, West, Grand Total. "east" groups into East
        // (first-seen display).
        assert_eq!(out.grid.len(), 4);
        assert_eq!(out.header_rows, 1);
        assert_eq!(out.grid[0], vec![s("Region"), s("Sum of Sales")]);
        assert_eq!(out.grid[1], vec![s("East"), n(80.0)]);
        assert_eq!(out.grid[2], vec![s("West"), n(90.0)]);
        assert_eq!(out.grid[3], vec![s("Grand Total"), n(170.0)]);
    }

    #[test]
    fn two_row_fields_and_two_measures() {
        let wb = sales_wb();
        let f = Frame::from_range(&wb, 0, (0, 0, 6, 3));
        let spec = PivotSpec {
            rows: vec![0, 1],
            measures: vec![
                Measure {
                    col: 3,
                    agg: Agg::Sum,
                    name: "Sum of Sales".into(),
                    calc: None,
                },
                Measure {
                    col: 2,
                    agg: Agg::Count,
                    name: "Count of Qty".into(),
                    calc: None,
                },
            ],
            grand_rows: true,
            ..PivotSpec::default()
        };
        let out = pivot(&f, &spec);
        assert_eq!(out.label_cols, 2);
        assert_eq!(
            out.grid[0],
            vec![
                s("Region"),
                s("Product"),
                s("Sum of Sales"),
                s("Count of Qty")
            ]
        );
        // Sorted: East/Ink, East/Pad, East/Pen, West/Pad, West/Pen.
        assert_eq!(out.grid[1], vec![s("East"), s("Ink"), n(10.0), n(1.0)]);
        assert_eq!(out.grid[2], vec![s("East"), s("Pad"), n(20.0), n(1.0)]);
        assert_eq!(out.grid[3], vec![s("East"), s("Pen"), n(50.0), n(2.0)]);
        assert_eq!(out.grid[4], vec![s("West"), s("Pad"), n(40.0), n(1.0)]);
        assert_eq!(out.grid[5], vec![s("West"), s("Pen"), n(50.0), n(1.0)]);
        assert_eq!(
            out.grid[6],
            vec![s("Grand Total"), Value::Empty, n(170.0), n(6.0)]
        );
    }

    #[test]
    fn measures_on_rows_stacks_them_vertically() {
        let wb = sales_wb();
        let f = Frame::from_range(&wb, 0, (0, 0, 6, 3));
        let spec = PivotSpec {
            rows: vec![0], // Region
            measures: vec![
                Measure {
                    col: 3,
                    agg: Agg::Sum,
                    name: "Sum of Sales".into(),
                    calc: None,
                },
                Measure {
                    col: 2,
                    agg: Agg::Sum,
                    name: "Sum of Qty".into(),
                    calc: None,
                },
            ],
            grand_rows: true,
            data_on_rows: true,
            ..PivotSpec::default()
        };
        let out = pivot(&f, &spec);
        // A measure-name column is added; each region gets one row per measure.
        assert_eq!(out.label_cols, 2);
        assert_eq!(out.grid[0], vec![s("Region"), Value::Empty, s("Total")]);
        // East (incl. "east"): Sales 30+20+20+10 = 80, Qty 3+1+2+4 = 10.
        assert_eq!(out.grid[1], vec![s("East"), s("Sum of Sales"), n(80.0)]);
        assert_eq!(out.grid[2], vec![Value::Empty, s("Sum of Qty"), n(10.0)]);
        // West: Sales 40+50 = 90, Qty 2+5 = 7.
        assert_eq!(out.grid[3], vec![s("West"), s("Sum of Sales"), n(90.0)]);
        assert_eq!(out.grid[4], vec![Value::Empty, s("Sum of Qty"), n(7.0)]);
        // Grand total, one row per measure.
        assert_eq!(
            out.grid[5],
            vec![s("Grand Total"), s("Sum of Sales"), n(170.0)]
        );
        assert_eq!(out.grid[6], vec![Value::Empty, s("Sum of Qty"), n(17.0)]);
    }

    #[test]
    fn crosstab_with_grand_totals() {
        let wb = sales_wb();
        let f = Frame::from_range(&wb, 0, (0, 0, 6, 3));
        let spec = PivotSpec {
            rows: vec![0],
            cols: vec![1],
            measures: vec![Measure {
                col: 3,
                agg: Agg::Sum,
                name: "Sum of Sales".into(),
                calc: None,
            }],
            grand_rows: true,
            grand_cols: true,
            ..PivotSpec::default()
        };
        let out = pivot(&f, &spec);
        assert_eq!(out.header_rows, 2);
        // Product values across the top, plus Grand Total.
        assert_eq!(
            out.grid[0],
            vec![Value::Empty, s("Ink"), s("Pad"), s("Pen"), s("Grand Total")]
        );
        assert_eq!(out.grid[2][0], s("East"));
        assert_eq!(out.grid[2][1], n(10.0)); // East/Ink
        assert_eq!(out.grid[2][2], n(20.0)); // East/Pad
        assert_eq!(out.grid[2][3], n(50.0)); // East/Pen
        assert_eq!(out.grid[2][4], n(80.0)); // East total
        assert_eq!(out.grid[3][1], Value::Empty); // West/Ink: no records
        assert_eq!(
            out.grid[4],
            vec![s("Grand Total"), n(10.0), n(60.0), n(100.0), n(170.0)]
        );
    }

    #[test]
    fn filters_and_aggregations() {
        let wb = sales_wb();
        let f = Frame::from_range(&wb, 0, (0, 0, 6, 3));
        let mk = |agg: Agg| PivotSpec {
            rows: vec![0],
            measures: vec![Measure {
                col: 2,
                agg,
                name: "m".into(),
                calc: None,
            }],
            filters: vec![(1, vec![s("pen")])], // case-insensitive
            ..PivotSpec::default()
        };
        let out = pivot(&f, &mk(Agg::Average));
        assert_eq!(out.grid[1], vec![s("East"), n(2.5)]); // (3+2)/2
        assert_eq!(out.grid[2], vec![s("West"), n(5.0)]);
        let out = pivot(&f, &mk(Agg::Max));
        assert_eq!(out.grid[1][1], n(3.0));
        // Max of all-negative values must stay negative.
        assert_eq!(Agg::Max.apply(&[n(-5.0), n(-3.0)]), n(-3.0));
        assert_eq!(Agg::Min.apply(&[n(-5.0), n(-3.0)]), n(-5.0));
        let out = pivot(&f, &mk(Agg::Min));
        assert_eq!(out.grid[2][1], n(5.0));
        let out = pivot(&f, &mk(Agg::Product));
        assert_eq!(out.grid[1][1], n(6.0));
        let out = pivot(&f, &mk(Agg::CountNums));
        assert_eq!(out.grid[1][1], n(2.0));
        // Sample variance of {3,2} = 0.5.
        let out = pivot(&f, &mk(Agg::Var));
        assert_eq!(out.grid[1][1], n(0.5));
        let out = pivot(&f, &mk(Agg::StdDevP));
        assert_eq!(out.grid[1][1], n(0.5)); // population stddev of {3,2}
        // Var of a single record is #DIV/0! (n-1 = 0).
        let out = pivot(&f, &mk(Agg::Var));
        assert_eq!(out.grid[2][1], Value::Err(ExcelError::Div0));
    }

    #[test]
    fn subtotal_rows_for_outer_groups() {
        let wb = sales_wb();
        let f = Frame::from_range(&wb, 0, (0, 0, 6, 3));
        let spec = PivotSpec {
            rows: vec![0, 1],
            measures: vec![Measure {
                col: 3,
                agg: Agg::Sum,
                name: "Sum of Sales".into(),
                calc: None,
            }],
            grand_rows: true,
            subtotals: true,
            ..PivotSpec::default()
        };
        let out = pivot(&f, &spec);
        let rows: Vec<(String, Value)> = out
            .grid
            .iter()
            .skip(1)
            .map(|r| {
                let label = match (&r[0], &r[1]) {
                    (Value::Str(a), Value::Empty) => a.clone(),
                    (Value::Str(a), Value::Str(b)) => format!("{a}/{b}"),
                    (Value::Empty, Value::Str(b)) => format!("·/{b}"),
                    _ => String::new(),
                };
                (label, r[2].clone())
            })
            .collect();
        // East group, East Total, West group, West Total, Grand Total.
        assert_eq!(rows[0].0, "East/Ink");
        assert_eq!(rows[3], ("East Total".to_string(), n(80.0)));
        assert_eq!(rows[4].0, "West/Pad");
        assert_eq!(rows[6], ("West Total".to_string(), n(90.0)));
        assert_eq!(rows[7], ("Grand Total".to_string(), n(170.0)));
        assert_eq!(rows.len(), 8);
    }

    #[test]
    fn frame_from_table_and_no_row_fields() {
        let mut wb = sales_wb();
        wb.tables.push(crate::sheet::Table {
            name: "Sales".to_string(),
            sheet: 0,
            range: (0, 0, 6, 3),
            header_rows: 1,
            totals_rows: 0,
            columns: vec![
                "Region".into(),
                "Product".into(),
                "Qty".into(),
                "Sales".into(),
            ],
            part: String::new(),
        });
        let f = Frame::from_table(&wb, "sales").unwrap();
        assert_eq!(f.rows(), 6);
        // No row fields: one Total row.
        let spec = PivotSpec {
            measures: vec![Measure {
                col: 3,
                agg: Agg::Sum,
                name: "Sum of Sales".into(),
                calc: None,
            }],
            ..PivotSpec::default()
        };
        let out = pivot(&f, &spec);
        assert_eq!(out.grid[1], vec![s("Total"), n(170.0)]);
    }

    #[test]
    fn delimiter_sniffing() {
        // Tab-separated.
        let f = Frame::from_csv("A\tB\n1\t2\n");
        assert_eq!(f.names, vec!["A", "B"]);
        assert_eq!(f.cols[1][0], n(2.0));
        // Semicolon-separated (European CSV).
        let f = Frame::from_csv("A;B\n1;2,5\n");
        assert_eq!(f.names, vec!["A", "B"]);
        assert_eq!(f.cols[1][0], Value::Str("2,5".into()));
        // Commas inside quotes don't confuse the sniffer.
        let f = Frame::from_csv("\"a,a\";B\nx;1\n");
        assert_eq!(f.names, vec!["a,a", "B"]);
        assert_eq!(sniff_delimiter("plain header\nrow\n"), ',');
    }

    #[test]
    fn agg_from_verb_name_covers_all_eleven_and_rejects_unknown() {
        let cases: &[(&str, Agg)] = &[
            ("sum", Agg::Sum),
            ("count", Agg::Count),
            ("countNums", Agg::CountNums),
            ("average", Agg::Average),
            ("max", Agg::Max),
            ("min", Agg::Min),
            ("product", Agg::Product),
            ("stdDev", Agg::StdDev),
            ("stdDevP", Agg::StdDevP),
            ("var", Agg::Var),
            ("varP", Agg::VarP),
        ];
        for (name, agg) in cases {
            assert_eq!(Agg::from_verb_name(name), Some(*agg), "{name}");
        }
        assert_eq!(Agg::from_verb_name("stdDevp"), None); // OOXML spelling, not the wire one
        assert_eq!(Agg::from_verb_name("bogus"), None);
    }

    #[test]
    fn pivot_spec_from_names_resolves_headers_and_builds_display_names() {
        let wb = sales_wb();
        let f = Frame::from_range(&wb, 0, (0, 0, 6, 3));
        let spec = pivot_spec_from_names(
            &f,
            &["Region".to_string()],
            &[],
            &[("Sales".to_string(), Agg::Sum)],
        )
        .unwrap();
        assert_eq!(spec.rows, vec![0]);
        assert!(spec.cols.is_empty());
        assert_eq!(spec.measures.len(), 1);
        assert_eq!(spec.measures[0].col, 3);
        assert_eq!(spec.measures[0].agg, Agg::Sum);
        assert_eq!(spec.measures[0].name, "Sum of Sales");
        // No grand totals by default — the ad-hoc pivot wire contract has no
        // flag for them.
        assert!(!spec.grand_rows);
        assert!(!spec.grand_cols);

        let out = pivot(&f, &spec);
        let table = pivot_table_strings(&out);
        assert_eq!(table[0], vec!["Region", "Sum of Sales"]);
        assert_eq!(table[1], vec!["East", "80"]);
        assert_eq!(table[2], vec!["West", "90"]);
    }

    #[test]
    fn pivot_spec_from_names_errors_naming_the_unknown_column() {
        let wb = sales_wb();
        let f = Frame::from_range(&wb, 0, (0, 0, 6, 3));
        let err = pivot_spec_from_names(
            &f,
            &["Region".to_string()],
            &[],
            &[("Bogus".to_string(), Agg::Sum)],
        )
        .unwrap_err();
        assert_eq!(err, "Bogus");
    }

    #[test]
    fn pivot_table_strings_renders_errors_and_blanks() {
        let out = PivotOut {
            grid: vec![vec![
                Value::Str("h".into()),
                Value::Empty,
                Value::Err(ExcelError::Div0),
                Value::Bool(true),
            ]],
            header_rows: 1,
            label_cols: 1,
        };
        assert_eq!(
            pivot_table_strings(&out)[0],
            vec!["h", "", "#DIV/0!", "TRUE"]
        );
    }

    #[test]
    fn range_stats_over_mixed_values() {
        let vals = vec![
            n(3.0),
            n(5.0),
            Value::Str("text".into()),
            Value::Empty,
            n(-2.0),
        ];
        let st = range_stats(&vals);
        assert_eq!(st.count, 4); // blanks excluded, text counted
        assert_eq!(st.count_nums, 3);
        assert_eq!(st.sum, 6.0);
        assert_eq!(st.average, 2.0);
        assert_eq!(st.min, -2.0);
        assert_eq!(st.max, 5.0);
    }

    #[test]
    fn range_stats_over_no_numbers_is_all_zero() {
        let st = range_stats(&[Value::Str("x".into()), Value::Empty]);
        assert_eq!(st.count, 1);
        assert_eq!(st.count_nums, 0);
        assert_eq!(st.sum, 0.0);
        assert_eq!(st.average, 0.0);
        assert_eq!(st.min, 0.0);
        assert_eq!(st.max, 0.0);
    }
}
