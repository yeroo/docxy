//! The data model: multiple tables, relationships, and measures over the
//! columnar query core — the headless BI layer.
//!
//! A [`DataModel`] holds named [`Frame`]s (from workbook Tables, CSV, or
//! built programmatically), **many-to-one relationships** between them
//! (`Sales[ProductID]` → `Products[ID]`), and named **measures** written in
//! ordinary Excel formula syntax over `Table[Column]` references —
//! `SUM(Sales[Amount])`, `SUMPRODUCT(Sales[Qty],Sales[Price])`,
//! `SUM(Sales[Amount])/SUM(Sales[Qty])`. The whole gridcore function
//! library is available inside measures.
//!
//! [`model_pivot`] evaluates measures per group with **filter context**:
//! grouping by a related dimension column (say `Products[Category]`)
//! filters the fact table through the relationship before each measure
//! evaluates — the star-schema behavior BI tools are built on.

use std::collections::HashMap;

use crate::formula::{self, Eval, ExcelError, TableInfo, Value};
use crate::frame::{Frame, PivotOut, key_of, keys_lt};

/// A many-to-one relationship: each `from` row matches at most one `to` row.
#[derive(Clone, Debug, PartialEq)]
pub struct Relationship {
    /// Many side: (table, column) — e.g. Sales[ProductID].
    pub from: (String, String),
    /// One side: (table, column) with unique keys — e.g. Products[ID].
    pub to: (String, String),
}

/// A named measure: an Excel formula over `Table[Column]` references.
#[derive(Clone, Debug)]
pub struct Measure {
    pub name: String,
    pub formula: String,
}

#[derive(Clone, Debug, Default)]
pub struct DataModel {
    pub tables: Vec<(String, Frame)>,
    pub relationships: Vec<Relationship>,
    pub measures: Vec<Measure>,
}

impl DataModel {
    /// Every Excel Table of a workbook becomes a model table.
    pub fn from_workbook(wb: &crate::sheet::Workbook) -> DataModel {
        let mut m = DataModel::default();
        for t in &wb.tables {
            if let Some(f) = Frame::from_table(wb, &t.name) {
                m.tables.push((t.name.clone(), f));
            }
        }
        m
    }

    pub fn add_table(&mut self, name: &str, frame: Frame) {
        self.tables.push((name.to_string(), frame));
    }

    pub fn add_csv(&mut self, name: &str, text: &str) {
        self.tables.push((name.to_string(), Frame::from_csv(text)));
    }

    pub fn table(&self, name: &str) -> Option<&Frame> {
        self.tables
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, f)| f)
    }

    fn table_index(&self, name: &str) -> Option<usize> {
        self.tables
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(name))
    }

    /// Add a many-to-one relationship. Errors when a table/column is
    /// missing or the one-side keys aren't unique (Excel's rule).
    pub fn relate(
        &mut self,
        from_table: &str,
        from_col: &str,
        to_table: &str,
        to_col: &str,
    ) -> Result<(), String> {
        let f = self
            .table(from_table)
            .ok_or_else(|| format!("unknown table {from_table}"))?;
        f.col_index(from_col)
            .ok_or_else(|| format!("unknown column {from_table}[{from_col}]"))?;
        let t = self
            .table(to_table)
            .ok_or_else(|| format!("unknown table {to_table}"))?;
        let key = t
            .col_index(to_col)
            .ok_or_else(|| format!("unknown column {to_table}[{to_col}]"))?;
        let mut seen = std::collections::HashSet::new();
        for v in &t.cols[key] {
            if !seen.insert(key_of(v)) {
                return Err(format!(
                    "{to_table}[{to_col}] must have unique values to be the one side"
                ));
            }
        }
        self.relationships.push(Relationship {
            from: (from_table.to_string(), from_col.to_string()),
            to: (to_table.to_string(), to_col.to_string()),
        });
        Ok(())
    }

    pub fn add_measure(&mut self, name: &str, formula: &str) {
        self.measures.push(Measure {
            name: name.to_string(),
            formula: formula.to_string(),
        });
    }

    /// The table's frame with every related dimension column joined in,
    /// following many→one relationships transitively (depth-capped).
    /// Joined columns are named `Table[Column]`; unmatched keys yield
    /// empties (like `RELATED` returning blank).
    pub fn expanded_frame(&self, table: &str) -> Option<Frame> {
        let base = self.table(table)?;
        let mut out = base.clone();
        self.expand_into(table, &mut out, &|c| c.to_string(), 0);
        Some(out)
    }

    fn expand_into(
        &self,
        table: &str,
        out: &mut Frame,
        local_col: &dyn Fn(&str) -> String,
        depth: u32,
    ) {
        if depth >= 8 {
            return;
        }
        for rel in &self.relationships {
            if !rel.from.0.eq_ignore_ascii_case(table) {
                continue;
            }
            let Some(dim) = self.table(&rel.to.0) else {
                continue;
            };
            let Some(dim_key) = dim.col_index(&rel.to.1) else {
                continue;
            };
            // The join key column as it appears in `out` (base columns keep
            // their names; nested hops go through the qualified name).
            let Some(fk) = out.col_index(&local_col(&rel.from.1)) else {
                continue;
            };
            // Hash the one side.
            let mut index: HashMap<String, usize> = HashMap::new();
            for (r, v) in dim.cols[dim_key].iter().enumerate() {
                index.entry(key_of(v)).or_insert(r);
            }
            let matches: Vec<Option<usize>> = out.cols[fk]
                .iter()
                .map(|v| index.get(&key_of(v)).copied())
                .collect();
            for (ci, cname) in dim.names.iter().enumerate() {
                let qualified = format!("{}[{}]", rel.to.0, cname);
                if out.col_index(&qualified).is_some() {
                    continue; // diamond/cycle: first join wins
                }
                out.names.push(qualified);
                out.cols.push(
                    matches
                        .iter()
                        .map(|m| match m {
                            Some(r) => dim.cols[ci][*r].clone(),
                            None => Value::Empty,
                        })
                        .collect(),
                );
            }
            // Follow the chain: the dimension may itself relate onward.
            let dim_name = rel.to.0.clone();
            self.expand_into(&dim_name, out, &|c| format!("{dim_name}[{c}]"), depth + 1);
        }
    }

    /// Evaluate a measure formula over the (unfiltered) model.
    pub fn eval_measure(&self, formula: &str) -> Value {
        eval_over(self, formula)
    }
}

/// The package part where model definitions persist across save/load.
/// A gridcore extension — Excel ignores it (and may drop it on its own
/// resave); our round-trip keeps it byte-for-byte like any other part.
pub const MODEL_PART: &str = "xl/gridcoreModel.xml";

/// Serialize a model's definitions (relationships + measures — tables come
/// from the workbook) into the custom part's XML.
pub fn model_part_xml(rels: &[Relationship], measures: &[Measure]) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<gridcoreModel xmlns=\"urn:gridcore:model\">",
    );
    for r in rels {
        out.push_str(&format!(
            "<relationship fromTable=\"{}\" fromCol=\"{}\" toTable=\"{}\" toCol=\"{}\"/>",
            esc(&r.from.0),
            esc(&r.from.1),
            esc(&r.to.0),
            esc(&r.to.1)
        ));
    }
    for m in measures {
        out.push_str(&format!(
            "<measure name=\"{}\" formula=\"{}\"/>",
            esc(&m.name),
            esc(&m.formula)
        ));
    }
    out.push_str("</gridcoreModel>");
    out
}

/// Parse the custom part back into definitions.
pub fn parse_model_part(xml: &str) -> (Vec<Relationship>, Vec<Measure>) {
    use opccore::xml::{Event, XmlParser};
    let mut rels = Vec::new();
    let mut measures = Vec::new();
    let mut p = XmlParser::new(xml);
    let decode = |raw: &str| {
        let mut s = String::new();
        XmlParser::append_decoded(raw, &mut s);
        s
    };
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "relationship" => rels.push(Relationship {
                    from: (decode(p.attr("fromTable")), decode(p.attr("fromCol"))),
                    to: (decode(p.attr("toTable")), decode(p.attr("toCol"))),
                }),
                "measure" => measures.push(Measure {
                    name: decode(p.attr("name")),
                    formula: decode(p.attr("formula")),
                }),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    (rels, measures)
}

/// Evaluate one formula against a model (virtual sheets = tables).
fn eval_over(model: &DataModel, src: &str) -> Value {
    let ast = match formula::parse(src) {
        Ok(a) => a,
        Err(_) => return Value::Err(ExcelError::Name),
    };
    let res = ModelResolver { model };
    let mut ev = Eval::new(&res, 0, (0, 0));
    ev.eval(&ast)
}

/// The formula evaluator's view of a model: each table is a virtual sheet
/// whose row 0 holds the headers, structured references resolve through the
/// table registry, and measure names behave like defined names.
struct ModelResolver<'a> {
    model: &'a DataModel,
}

impl formula::Resolver for ModelResolver<'_> {
    fn value(&self, sheet: usize, row: u32, col: u32) -> Value {
        let Some((_, f)) = self.model.tables.get(sheet) else {
            return Value::Empty;
        };
        let c = col as usize;
        if c >= f.cols.len() {
            return Value::Empty;
        }
        if row == 0 {
            return Value::Str(f.names[c].clone());
        }
        f.cols[c]
            .get(row as usize - 1)
            .cloned()
            .unwrap_or(Value::Empty)
    }

    fn sheet_index(&self, name: &str) -> Option<usize> {
        self.model.table_index(name)
    }

    fn cells_in(
        &self,
        sheet: usize,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
    ) -> Vec<((u32, u32), Value)> {
        let mut out = Vec::new();
        let Some((_, f)) = self.model.tables.get(sheet) else {
            return out;
        };
        let (rows, cols) = (f.rows() as u32 + 1, f.cols.len() as u32);
        for r in r1..=r2.min(rows.saturating_sub(1)) {
            for c in c1..=c2.min(cols.saturating_sub(1)) {
                let v = self.value(sheet, r, c);
                if !matches!(v, Value::Empty) {
                    out.push(((r, c), v));
                }
            }
        }
        out
    }

    fn used_size(&self, sheet: usize) -> (u32, u32) {
        match self.model.tables.get(sheet) {
            Some((_, f)) => (f.rows() as u32 + 1, f.cols.len() as u32),
            None => (0, 0),
        }
    }

    fn table(&self, name: &str) -> Option<TableInfo> {
        let idx = self.model.table_index(name)?;
        let f = &self.model.tables[idx].1;
        Some(TableInfo {
            sheet: idx,
            range: (0, 0, f.rows() as u32, f.cols.len().saturating_sub(1) as u32),
            header_rows: 1,
            totals_rows: 0,
            columns: f.names.clone(),
        })
    }

    fn table_at(&self, sheet: usize, row: u32, col: u32) -> Option<TableInfo> {
        // Row-context iteration positions the evaluator inside a virtual
        // table sheet; any in-range cell belongs to that table.
        let (_, f) = self.model.tables.get(sheet)?;
        let (rows, cols) = (f.rows() as u32, f.cols.len() as u32);
        if row <= rows && col < cols {
            let name = &self.model.tables[sheet].0;
            return self.table(name);
        }
        None
    }

    fn defined_name(&self, name: &str, _current_sheet: usize) -> Option<String> {
        self.model
            .measures
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
            .map(|m| m.formula.clone())
    }
}

/// A model pivot: group by columns of the base table's *expanded* frame
/// (base columns by name, related ones as `Table[Column]`), and evaluate
/// measures per group under that filter context.
#[derive(Clone, Debug, Default)]
pub struct ModelSpec {
    pub rows: Vec<String>,
    pub cols: Vec<String>,
    /// (display name, formula) — or a bare measure name defined on the model.
    pub measures: Vec<(String, String)>,
    pub grand_rows: bool,
    pub grand_cols: bool,
}

/// Group the base (fact) table by the spec's fields and evaluate each
/// measure per group, with the group's rows as the fact table's filter
/// context.
pub fn model_pivot(model: &DataModel, base: &str, spec: &ModelSpec) -> Result<PivotOut, String> {
    let expanded = model
        .expanded_frame(base)
        .ok_or_else(|| format!("unknown table {base}"))?;
    let col_of = |n: &String| {
        expanded
            .col_index(n)
            .ok_or_else(|| format!("unknown field {n}"))
    };
    let row_fields: Vec<usize> = spec.rows.iter().map(col_of).collect::<Result<_, _>>()?;
    let col_fields: Vec<usize> = spec.cols.iter().map(col_of).collect::<Result<_, _>>()?;
    let base_idx = model
        .table_index(base)
        .ok_or_else(|| format!("unknown table {base}"))?;

    // Distinct sorted key combos, canonicalized per field (as frame::pivot).
    let nrows = expanded.rows();
    let canon = |c: usize, r: usize| -> Value {
        let key = key_of(&expanded.cols[c][r]);
        for rr in 0..nrows {
            if key_of(&expanded.cols[c][rr]) == key {
                return expanded.cols[c][rr].clone();
            }
        }
        expanded.cols[c][r].clone()
    };
    let combos = |fields: &[usize]| -> Vec<Vec<Value>> {
        if fields.is_empty() {
            return vec![Vec::new()];
        }
        let mut seen: Vec<(String, Vec<Value>)> = Vec::new();
        for r in 0..nrows {
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
    let row_combos = combos(&row_fields);
    let col_combos = combos(&col_fields);

    let combo_key = |fields: &[usize], r: usize| -> String {
        fields
            .iter()
            .map(|&c| key_of(&expanded.cols[c][r]))
            .collect()
    };
    let row_keys: Vec<String> = row_combos
        .iter()
        .map(|c| c.iter().map(key_of).collect())
        .collect();
    let col_keys: Vec<String> = col_combos
        .iter()
        .map(|c| c.iter().map(key_of).collect())
        .collect();
    let mut buckets: Vec<Vec<Vec<usize>>> =
        vec![vec![Vec::new(); col_combos.len()]; row_combos.len()];
    for r in 0..nrows {
        let rk = combo_key(&row_fields, r);
        let ck = combo_key(&col_fields, r);
        let ri = row_keys.iter().position(|k| *k == rk).unwrap_or(0);
        let ci = col_keys.iter().position(|k| *k == ck).unwrap_or(0);
        buckets[ri][ci].push(r);
    }

    // Evaluate every measure under a bucket's filter context: the fact
    // table restricted to the bucket's rows.
    let eval_bucket = |records: &[usize], mformula: &str| -> Value {
        if records.is_empty() {
            return Value::Empty;
        }
        let mut sub = model.clone();
        let (_, fact) = &model.tables[base_idx];
        let filtered = Frame {
            names: fact.names.clone(),
            cols: fact
                .cols
                .iter()
                .map(|col| records.iter().map(|&r| col[r].clone()).collect())
                .collect(),
        };
        sub.tables[base_idx].1 = filtered;
        // A bare name is a model measure; anything else is a formula.
        let src = sub
            .measures
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(mformula))
            .map(|m| m.formula.clone())
            .unwrap_or_else(|| mformula.to_string());
        eval_over(&sub, &src)
    };

    // Layout — same shape as frame::pivot.
    let m = spec.measures.len().max(1);
    let label_cols = row_fields.len().max(1);
    let header_rows = col_fields.len() + 1;
    let grand_col = spec.grand_cols && !col_fields.is_empty();
    let total_cols = label_cols + col_combos.len() * m + if grand_col { m } else { 0 };

    let mut grid: Vec<Vec<Value>> = Vec::new();
    for h in 0..col_fields.len() {
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
    {
        let mut row = vec![Value::Empty; total_cols];
        for (i, name) in spec.rows.iter().enumerate() {
            row[i] = Value::Str(name.clone());
        }
        for ci in 0..col_combos.len() + usize::from(grand_col) {
            for (k, (name, _)) in spec.measures.iter().enumerate() {
                row[label_cols + ci * m + k] = Value::Str(name.clone());
            }
        }
        grid.push(row);
    }
    for (ri, combo) in row_combos.iter().enumerate() {
        let mut row = vec![Value::Empty; total_cols];
        if combo.is_empty() {
            row[0] = Value::Str("Total".into());
        }
        for (i, v) in combo.iter().enumerate() {
            row[i] = v.clone();
        }
        for ci in 0..col_combos.len() {
            for (k, (_, f)) in spec.measures.iter().enumerate() {
                row[label_cols + ci * m + k] = eval_bucket(&buckets[ri][ci], f);
            }
        }
        if grand_col {
            let all: Vec<usize> = buckets[ri].iter().flatten().copied().collect();
            for (k, (_, f)) in spec.measures.iter().enumerate() {
                row[label_cols + col_combos.len() * m + k] = eval_bucket(&all, f);
            }
        }
        grid.push(row);
    }
    if spec.grand_rows && !row_combos.is_empty() && !row_fields.is_empty() {
        let mut row = vec![Value::Empty; total_cols];
        row[0] = Value::Str("Grand Total".into());
        for ci in 0..col_combos.len() {
            let col_all: Vec<usize> = buckets.iter().flat_map(|b| b[ci].iter().copied()).collect();
            for (k, (_, f)) in spec.measures.iter().enumerate() {
                row[label_cols + ci * m + k] = eval_bucket(&col_all, f);
            }
        }
        if grand_col {
            let all: Vec<usize> = buckets.iter().flatten().flatten().copied().collect();
            for (k, (_, f)) in spec.measures.iter().enumerate() {
                row[label_cols + col_combos.len() * m + k] = eval_bucket(&all, f);
            }
        }
        grid.push(row);
    }

    Ok(PivotOut {
        grid,
        header_rows,
        label_cols,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f64) -> Value {
        Value::Num(x)
    }
    fn t(s: &str) -> Value {
        Value::Str(s.to_string())
    }

    /// Sales(fact) -> Products(dim) -> Categories(dim): a two-hop star.
    fn star() -> DataModel {
        let mut m = DataModel::default();
        m.add_csv(
            "Sales",
            "ProductID,Qty,Amount\n1,2,20\n2,1,15\n1,3,30\n3,5,50\n",
        );
        m.add_csv(
            "Products",
            "ID,Name,GroupID\n1,Pen,10\n2,Pad,10\n3,Ink,20\n",
        );
        m.add_csv("Groups", "GID,Category\n10,Office\n20,Supplies\n");
        m.relate("Sales", "ProductID", "Products", "ID").unwrap();
        m.relate("Products", "GroupID", "Groups", "GID").unwrap();
        m
    }

    #[test]
    fn csv_parsing_types_and_quotes() {
        let f = Frame::from_csv(
            "Name,Qty,\"Unit, Price\",Active\n\"Smith, John\",3,2.5,true\n\"He said \"\"hi\"\"\",,-1e2,FALSE\r\n",
        );
        assert_eq!(f.names, vec!["Name", "Qty", "Unit, Price", "Active"]);
        assert_eq!(f.rows(), 2);
        assert_eq!(f.cols[0][0], t("Smith, John"));
        assert_eq!(f.cols[1][0], v(3.0));
        assert_eq!(f.cols[2][0], v(2.5));
        assert_eq!(f.cols[3][0], Value::Bool(true));
        assert_eq!(f.cols[0][1], t("He said \"hi\""));
        assert_eq!(f.cols[1][1], Value::Empty);
        assert_eq!(f.cols[2][1], v(-100.0));
        assert_eq!(f.cols[3][1], Value::Bool(false));
    }

    #[test]
    fn relationships_validate() {
        let mut m = star();
        // Non-unique one side is rejected.
        m.add_csv("Dup", "ID,X\n1,a\n1,b\n");
        assert!(m.relate("Sales", "ProductID", "Dup", "ID").is_err());
        assert!(m.relate("Sales", "Nope", "Products", "ID").is_err());
        assert!(m.relate("Sales", "ProductID", "Nope", "ID").is_err());
    }

    #[test]
    fn expanded_frame_joins_transitively() {
        let m = star();
        let f = m.expanded_frame("Sales").unwrap();
        // Base columns plus Products[...] plus Groups[...] (two hops).
        let name_col = f.col_index("Products[Name]").unwrap();
        let cat_col = f.col_index("Groups[Category]").unwrap();
        assert_eq!(f.cols[name_col][0], t("Pen"));
        assert_eq!(f.cols[name_col][3], t("Ink"));
        assert_eq!(f.cols[cat_col][0], t("Office"));
        assert_eq!(f.cols[cat_col][3], t("Supplies"));
        // Unmatched key -> blank, like RELATED().
        let mut m2 = star();
        m2.tables[0].1.cols[0][0] = v(99.0); // ProductID with no product
        let f2 = m2.expanded_frame("Sales").unwrap();
        let name_col = f2.col_index("Products[Name]").unwrap();
        assert_eq!(f2.cols[name_col][0], Value::Empty);
    }

    #[test]
    fn measures_are_excel_formulas_over_tables() {
        let mut m = star();
        assert_eq!(m.eval_measure("SUM(Sales[Amount])"), v(115.0));
        assert_eq!(
            m.eval_measure("SUMPRODUCT(Sales[Qty],Sales[Amount])"),
            v(395.0)
        );
        assert_eq!(m.eval_measure("COUNTA(Products[Name])"), v(3.0));
        assert_eq!(
            m.eval_measure("SUMIFS(Sales[Amount],Sales[ProductID],1)"),
            v(50.0)
        );
        assert_eq!(
            m.eval_measure("MAX(Sales[Amount])-MIN(Sales[Amount])"),
            v(35.0)
        );
        // Named measures compose (a measure referencing another measure).
        m.add_measure("TotalAmount", "SUM(Sales[Amount])");
        m.add_measure("AvgPerUnit", "TotalAmount/SUM(Sales[Qty])");
        assert_eq!(m.eval_measure("TotalAmount"), v(115.0));
        assert_eq!(m.eval_measure("AvgPerUnit"), v(115.0 / 11.0));
        // Unknown names surface as #NAME?.
        assert_eq!(m.eval_measure("Nonsense+1"), Value::Err(ExcelError::Name));
    }

    #[test]
    fn sumx_measures_respect_filter_context() {
        let mut m = star();
        // Row-context measure: revenue-weighted... just Qty*Amount per row.
        m.add_measure("Weighted", "SUMX(Sales,[@Qty]*[@Amount])");
        assert_eq!(
            m.eval_measure("Weighted"),
            v(2.0 * 20.0 + 15.0 + 3.0 * 30.0 + 5.0 * 50.0)
        );
        // Under filter context, only the group's fact rows iterate.
        let spec = ModelSpec {
            rows: vec!["Groups[Category]".into()],
            measures: vec![("W".into(), "Weighted".into())],
            grand_rows: true,
            ..ModelSpec::default()
        };
        let out = model_pivot(&m, "Sales", &spec).unwrap();
        // Office = products 1,2: 2*20 + 1*15 + 3*30 = 145; Supplies = 5*50.
        assert_eq!(out.grid[1], vec![t("Office"), v(145.0)]);
        assert_eq!(out.grid[2], vec![t("Supplies"), v(250.0)]);
        assert_eq!(out.grid[3], vec![t("Grand Total"), v(395.0)]);
    }

    #[test]
    fn model_pivot_filters_through_relationships() {
        let mut m = star();
        m.add_measure("Total", "SUM(Sales[Amount])");
        let spec = ModelSpec {
            rows: vec!["Groups[Category]".into()],
            measures: vec![
                ("Total".into(), "Total".into()),
                ("Units".into(), "SUM(Sales[Qty])".into()),
            ],
            grand_rows: true,
            ..ModelSpec::default()
        };
        let out = model_pivot(&m, "Sales", &spec).unwrap();
        // Office = products 1,2 -> 20+15+30 = 65 amount, 6 qty;
        // Supplies = product 3 -> 50 amount, 5 qty.
        assert_eq!(
            out.grid[0],
            vec![t("Groups[Category]"), t("Total"), t("Units")]
        );
        assert_eq!(out.grid[1], vec![t("Office"), v(65.0), v(6.0)]);
        assert_eq!(out.grid[2], vec![t("Supplies"), v(50.0), v(5.0)]);
        assert_eq!(out.grid[3], vec![t("Grand Total"), v(115.0), v(11.0)]);
    }

    #[test]
    fn model_pivot_crosstab_with_ratio_measure() {
        let m = star();
        let spec = ModelSpec {
            rows: vec!["Groups[Category]".into()],
            cols: vec!["Products[Name]".into()],
            measures: vec![(
                "AvgPrice".into(),
                "SUM(Sales[Amount])/SUM(Sales[Qty])".into(),
            )],
            grand_rows: true,
            grand_cols: true,
        };
        let out = model_pivot(&m, "Sales", &spec).unwrap();
        // Columns sorted: Ink, Pad, Pen (+ Grand Total).
        assert_eq!(
            out.grid[0],
            vec![Value::Empty, t("Ink"), t("Pad"), t("Pen"), t("Grand Total")]
        );
        // Office row: Ink empty, Pad 15/1, Pen 50/5.
        assert_eq!(out.grid[2][0], t("Office"));
        assert_eq!(out.grid[2][1], Value::Empty);
        assert_eq!(out.grid[2][2], v(15.0));
        assert_eq!(out.grid[2][3], v(10.0));
        assert_eq!(out.grid[2][4], v(65.0 / 6.0));
        // Supplies row: only Ink, 50/5.
        assert_eq!(out.grid[3][0], t("Supplies"));
        assert_eq!(out.grid[3][1], v(10.0));
        assert_eq!(out.grid[3][4], v(10.0));
        // Grand Total row.
        assert_eq!(out.grid[4][1], v(10.0)); // Ink: 50/5
        assert_eq!(out.grid[4][4], v(115.0 / 11.0));
    }

    #[test]
    fn model_part_round_trips_definitions() {
        let rels = vec![Relationship {
            from: ("Sales".into(), "ProductID".into()),
            to: ("Products".into(), "ID".into()),
        }];
        let measures = vec![Measure {
            name: "Total".into(),
            formula: "SUM(Sales[Amount])<>\"\"&\"x\"".into(), // XML-hostile
        }];
        let xml = model_part_xml(&rels, &measures);
        let (r2, m2) = parse_model_part(&xml);
        assert_eq!(r2, rels);
        assert_eq!(m2[0].name, "Total");
        assert_eq!(m2[0].formula, measures[0].formula);
    }

    #[test]
    fn from_workbook_picks_up_tables() {
        use crate::sheet::{Cell, Sheet, Table, Workbook};
        let mut sh = Sheet {
            name: "Data".into(),
            ..Sheet::default()
        };
        for (c, h) in ["K", "V"].iter().enumerate() {
            sh.set_cell(0, c as u32, Cell::text(h));
        }
        sh.set_cell(1, 0, Cell::number(1.0));
        sh.set_cell(1, 1, Cell::number(7.0));
        let mut wb = Workbook {
            sheets: vec![sh],
            ..Workbook::default()
        };
        wb.tables.push(Table {
            name: "T".into(),
            sheet: 0,
            range: (0, 0, 1, 1),
            header_rows: 1,
            totals_rows: 0,
            columns: vec!["K".into(), "V".into()],
            part: String::new(),
        });
        let m = DataModel::from_workbook(&wb);
        assert_eq!(m.eval_measure("SUM(T[V])"), v(7.0));
    }
}
