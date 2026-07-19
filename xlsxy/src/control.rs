//! The xlsxy control surface: maps [`ctlcore`] verbs onto the **live** workbook,
//! so an external agent (e.g. Claude Code in a sibling agwinterm pane) can read
//! and edit the open spreadsheet without touching the file on disk.
//!
//! Every mutating verb goes through the app's own undoable edit path
//! ([`App::apply_on`]), so an agent's edits land on the *same* undo stack as
//! keyboard edits, recalculate dependents, and repaint the view live; reads
//! serialize the in-memory workbook, so they always reflect unsaved changes.
//!
//! Addressing is A1-style: `ref` is a single cell (`B4`), `range` is `A1:C10`
//! (or a single cell). `sheet` selects a sheet by index or name and defaults to
//! the active one.
//!
//! ## Verbs
//!
//! | Verb | Args | Result |
//! |---|---|---|
//! | `wb.path` | — | `{path, modified, sheets, active, active_name}` |
//! | `sheet.list` | — | `{active, sheets:[{index, name, rows, cols}]}` |
//! | `sheet.read` | `{sheet?, range?}` | `{sheet, name, rows, cols, cells:[…], truncated}` |
//! | `cell.get` | `{ref, sheet?}` | `{ref, row, col, value, formula?, text}` |
//! | `cell.set` | `{ref, text, sheet?}` | `{ref, value, text, …}` |
//! | `range.clear` | `{range, sheet?}` | `{cleared}` |
//! | `find` | `{query, sheet?}` | `{count, matches:[…]}` |
//! | `comment.list` | — | `{comments:[{sheet,ref,author,text}]}` |
//! | `wb.export-csv` | `{sheet?}` | `{sheet, csv}` (display-formatted) |
//! | `sheet.pivot` | `{range,rows,cols?,values,sheet?}` | `{table:[[string]]}` — read-only |
//! | `formula.eval` | `{formula,ref?,sheet?}` | `{value,text}` — side-effect-free |
//! | `sheet.stats` | `{range,sheet?}` | `{sum,count,countNums,average,min,max}` |
//! | `chart.list` | — | `{charts:[{kind,title?,categories,series:[{name?,values}]}]}` |
//! | `pivot.list` | — | `{pivots:[{sheet,rows,cols,values}]}` |
//! | `pivot.create` | `{range,rows,cols?,values,name?,sheet?}` | `{sheet,name}` — REAL persistent pivot on a NEW sheet; clears undo history like `sheet.add` |
//! | `comment.add` | `{ref,text,author?,sheet?}` | `{sheet,ref}` |
//! | `comment.remove` | `{ref,sheet?}` | `{removed:bool}` |
//! | `range.set` | `{start,rows:[[string]],sheet?}` | `{set}` — atomic, one undo group |
//! | `sheet.import-csv` | `{text,name?}` | `{sheet,name,rows,cols}` — always a new sheet |
//! | `wb.replace-all` | `{query,text}` | `{replaced}` — every sheet, one undo group |
//! | `sheet.add` | `{name?}` | `{sheet,name}` |
//! | `sheet.remove` | `{sheet}` | `{removed:true}` (last-sheet error) |
//! | `sheet.rename` | `{sheet,name}` | `{name}` |
//! | `row.insert` / `row.delete` | `{at,count?,sheet?}` | `{inserted\|deleted}` |
//! | `col.insert` / `col.delete` | `{at,count?,sheet?}` | `{inserted\|deleted}` |
//! | `cell.format` | `{range,patch,sheet?}` | `{formatted}` — one undo group; `patch` keys: `numFmt`/`bold`/`italic`/`fontColor`/`fillColor`/`align` (≥1 required) |
//! | `col.width` | `{col,width,sheet?}` | `{col,width}` — NOT on the undo stack (mirrors the TUI's F7/F8, which mutate directly) |
//! | `wb.recalc` | — | `{recalculated:true}` |
//! | `wb.save` | — | `{path, …}` |
//! | `wb.reload` | — | `{path, …}` |
//! | `wb.open` | `{path}` | `{path, …}` |
//!
//! `cell.get`'s reply additively gains a `format` object — present only when
//! the cell's style differs from the default in at least one of the six
//! `cell.format` keys; see [`gridcore::format::xf_format_fields`]. This is
//! deliberately scoped to `cell.get` alone (read-modify-write is the only
//! use case): `sheet.read`, `find`, and `cell.set`'s reply share the same
//! underlying `cell_json` builder but do NOT carry `format` — a fully-styled
//! `sheet.read` window can return thousands of cells, and paying six extra
//! keys per cell there (or on the busiest mutating verb, `cell.set`) isn't
//! worth it when nothing consumes it.

use crate::{App, comment_author, iso_now, parse_input};
use ctlcore::json::Json;
use gridcore::engine::{Engine, cell_to_value, eval_formula_at};
use gridcore::format::{FormatPatch, FormatValue, apply_patch_to_xf, xf_format_fields};
use gridcore::formula::Value;
use gridcore::frame::{Agg, Frame, pivot, pivot_spec_from_names, pivot_table_strings, range_stats};
use gridcore::sheet::{
    Cell, CellValue, DrawingKind, Styles, cell_name, fmt_general, parse_cell_name, parse_col,
    parse_range_name, sheet_to_csv,
};

/// The most cells one `sheet.read` returns (non-empty cells in the window);
/// larger reads set `truncated: true` so a client narrows the range.
const READ_CAP: usize = 5000;
/// The most matches one `find` returns.
const FIND_CAP: usize = 200;

/// Route one control verb against the live workbook, returning the JSON result
/// or an error message.
pub fn dispatch(app: &mut App, verb: &str, args: &Json) -> Result<Json, String> {
    let out = match verb {
        "wb.path" => Ok(path_info(app)),
        "sheet.list" => Ok(sheet_list(app)),
        "sheet.read" => sheet_read(app, args),
        "cell.get" => cell_get(app, args),
        "cell.set" => cell_set(app, args),
        "range.clear" => range_clear(app, args),
        "find" => find(app, args),
        "comment.list" => Ok(comment_list(app)),
        "wb.export-csv" => wb_export_csv(app, args),
        "sheet.pivot" => sheet_pivot(app, args),
        "formula.eval" => formula_eval(app, args),
        "sheet.stats" => sheet_stats(app, args),
        "chart.list" => Ok(chart_list(app)),
        "pivot.list" => Ok(pivot_list(app)),
        "pivot.create" => pivot_create(app, args),
        "comment.add" => comment_add(app, args),
        "comment.remove" => comment_remove(app, args),
        "range.set" => range_set(app, args),
        "sheet.import-csv" => sheet_import_csv(app, args),
        "wb.replace-all" => wb_replace_all(app, args),
        "sheet.add" => sheet_add(app, args),
        "sheet.remove" => sheet_remove(app, args),
        "sheet.rename" => sheet_rename(app, args),
        "row.insert" => row_op(app, args, true),
        "row.delete" => row_op(app, args, false),
        "col.insert" => col_op(app, args, true),
        "col.delete" => col_op(app, args, false),
        "cell.format" => cell_format(app, args),
        "col.width" => col_width(app, args),
        "wb.recalc" => {
            app.recalc_and_refresh();
            Ok(Json::obj(vec![("recalculated", Json::Bool(true))]))
        }
        "wb.save" => {
            app.save();
            Ok(path_info(app))
        }
        "wb.reload" => {
            let p = app.path.clone();
            app.open_workbook(&p);
            Ok(path_info(app))
        }
        "wb.open" => {
            let p = args
                .get_str("path")
                .ok_or("wb.open needs a 'path' string")?
                .to_string();
            app.open_workbook(&p);
            Ok(path_info(app))
        }
        other => Err(format!("unknown verb '{other}'")),
    };
    if out.is_ok() {
        // An agent edit flashes this pane's status dot, so a watcher sees the
        // workbook being worked on.
        if matches!(
            verb,
            "cell.set"
                | "range.clear"
                | "comment.add"
                | "range.set"
                | "sheet.import-csv"
                | "wb.replace-all"
                | "sheet.add"
                | "sheet.remove"
                | "sheet.rename"
                | "pivot.create"
                | "row.insert"
                | "row.delete"
                | "col.insert"
                | "col.delete"
                | "cell.format"
                | "col.width"
        ) {
            ctlcore::signal_activity();
        }
        // `comment.remove` can legitimately no-op (nothing on the cell), so it
        // signals itself inside `comment_remove`, gated on `removed:true` — a
        // no-op must not flash the activity dot (docxy's no-op principle).
    }
    out
}

// ---------------------------------------------------------------------------
// Read-only verbs
// ---------------------------------------------------------------------------

fn path_info(app: &App) -> Json {
    let wb = &app.pkg.workbook;
    Json::obj(vec![
        ("path", Json::Str(app.path.clone())),
        ("modified", Json::Bool(app.modified)),
        ("sheets", Json::Num(wb.sheets.len() as f64)),
        ("active", Json::Num(app.sheet as f64)),
        ("active_name", Json::Str(wb.sheets[app.sheet].name.clone())),
    ])
}

fn sheet_list(app: &App) -> Json {
    let sheets = app
        .pkg
        .workbook
        .sheets
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (rows, cols) = s.used_size();
            Json::obj(vec![
                ("index", Json::Num(i as f64)),
                ("name", Json::Str(s.name.clone())),
                ("rows", Json::Num(rows as f64)),
                ("cols", Json::Num(cols as f64)),
            ])
        })
        .collect();
    Json::obj(vec![
        ("active", Json::Num(app.sheet as f64)),
        ("sheets", Json::Arr(sheets)),
    ])
}

/// One cell as JSON: `ref`, coordinates, the typed `value`, the formula
/// source (with `=`), and `text` (the general-format display string). No
/// `format` key — see [`cell_json_with_format`], used by `cell.get` alone.
fn cell_json(row: u32, col: u32, cell: &Cell) -> Json {
    let mut fields = vec![
        ("ref", Json::Str(cell_name(row, col))),
        ("row", Json::Num(row as f64)),
        ("col", Json::Num(col as f64)),
        ("value", value_json(&cell.value)),
        ("text", Json::Str(value_text(&cell.value))),
    ];
    if let Some(f) = &cell.formula {
        fields.push(("formula", Json::Str(format!("={f}"))));
    }
    Json::obj(fields)
}

/// [`cell_json`] plus — additively, present only when set — a `format`
/// object (see [`format_json`]). Deliberately used by `cell.get` ONLY: the
/// read-back exists for read-modify-write, not for bulk reads
/// (`sheet.read`/`find`) or the busiest mutating verb (`cell.set`), which
/// all still go through the plain [`cell_json`] above.
fn cell_json_with_format(row: u32, col: u32, cell: &Cell, styles: &Styles) -> Json {
    let mut j = cell_json(row, col, cell);
    if let Some(fmt) = format_json(styles, cell.style) {
        if let Json::Obj(pairs) = &mut j {
            pairs.push(("format".to_string(), fmt));
        }
    }
    j
}

/// The `cell.get` read-back `format` object for style index `style`: only
/// the `cell.format` patch keys whose value differs from the default style,
/// via [`xf_format_fields`]; `None` for an unstyled cell (no `format` key on
/// the wire at all).
fn format_json(styles: &Styles, style: u32) -> Option<Json> {
    let xf = styles.xf(style);
    let fields = xf_format_fields(&xf);
    if fields.is_empty() {
        return None;
    }
    let pairs = fields
        .into_iter()
        .map(|(k, v)| {
            let v = match v {
                FormatValue::Str(s) => Json::Str(s),
                FormatValue::Bool(b) => Json::Bool(b),
            };
            (k.to_string(), v)
        })
        .collect();
    Some(Json::Obj(pairs))
}

fn value_json(v: &CellValue) -> Json {
    match v {
        CellValue::Empty => Json::Null,
        CellValue::Number(n) => Json::Num(*n),
        CellValue::Text(s) => Json::Str(s.clone()),
        CellValue::Bool(b) => Json::Bool(*b),
        CellValue::Error(e) => Json::Str(e.clone()),
    }
}

fn value_text(v: &CellValue) -> String {
    match v {
        CellValue::Empty => String::new(),
        CellValue::Number(n) => fmt_general(*n),
        CellValue::Text(s) => s.clone(),
        CellValue::Bool(true) => "TRUE".to_string(),
        CellValue::Bool(false) => "FALSE".to_string(),
        CellValue::Error(e) => e.clone(),
    }
}

fn sheet_read(app: &App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let s = &app.pkg.workbook.sheets[si];
    let (used_r, used_c) = s.used_size();
    let (r1, c1, r2, c2) = match args.get_str("range") {
        Some(rg) => parse_range(rg)?,
        // Whole used range (empty sheet → the single cell A1).
        None => (0, 0, used_r.saturating_sub(1), used_c.saturating_sub(1)),
    };
    let mut cells = Vec::new();
    let mut truncated = false;
    for (&(r, c), cell) in s.cells.range((r1, 0)..=(r2, u32::MAX)) {
        if c < c1 || c > c2 || cell.is_blank() {
            continue;
        }
        if cells.len() >= READ_CAP {
            truncated = true;
            break;
        }
        cells.push(cell_json(r, c, cell));
    }
    Ok(Json::obj(vec![
        ("sheet", Json::Num(si as f64)),
        ("name", Json::Str(s.name.clone())),
        ("rows", Json::Num(used_r as f64)),
        ("cols", Json::Num(used_c as f64)),
        ("cells", Json::Arr(cells)),
        ("truncated", Json::Bool(truncated)),
    ]))
}

fn cell_get(app: &App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let (r, c) = ref_arg(args)?;
    let s = &app.pkg.workbook.sheets[si];
    match s.cell(r, c) {
        Some(cell) => Ok(cell_json_with_format(r, c, cell, &app.pkg.workbook.styles)),
        None => Ok(Json::obj(vec![
            ("ref", Json::Str(cell_name(r, c))),
            ("row", Json::Num(r as f64)),
            ("col", Json::Num(c as f64)),
            ("value", Json::Null),
            ("text", Json::Str(String::new())),
        ])),
    }
}

fn find(app: &App, args: &Json) -> Result<Json, String> {
    let query = args.get_str("query").ok_or("find needs a 'query'")?;
    if query.is_empty() {
        return Err("empty query".into());
    }
    let needle = query.to_lowercase();
    // A `sheet` arg restricts the search; default is every sheet.
    let only: Option<usize> = match args.get("sheet") {
        Some(_) => Some(sheet_arg(app, args)?),
        None => None,
    };
    let mut matches = Vec::new();
    'outer: for (si, s) in app.pkg.workbook.sheets.iter().enumerate() {
        if only.is_some_and(|o| o != si) {
            continue;
        }
        for (&(r, c), cell) in &s.cells {
            let text_hit = value_text(&cell.value).to_lowercase().contains(&needle);
            let formula_hit = cell
                .formula
                .as_deref()
                .is_some_and(|f| f.to_lowercase().contains(&needle));
            if text_hit || formula_hit {
                if matches.len() >= FIND_CAP {
                    break 'outer;
                }
                let mut m = cell_json(r, c, cell);
                if let Json::Obj(pairs) = &mut m {
                    pairs.insert(0, ("sheet".to_string(), Json::Num(si as f64)));
                    pairs.insert(1, ("sheet_name".to_string(), Json::Str(s.name.clone())));
                }
                matches.push(m);
            }
        }
    }
    Ok(Json::obj(vec![
        ("query", Json::Str(query.to_string())),
        ("count", Json::Num(matches.len() as f64)),
        ("matches", Json::Arr(matches)),
    ]))
}

/// Every comment in the workbook, flattened in `SheetPackage::comments`'s
/// reply order (sheet, then row, then column).
fn comment_list(app: &App) -> Json {
    let comments = app
        .pkg
        .comments()
        .iter()
        .map(|c| {
            Json::obj(vec![
                ("sheet", Json::Num(c.sheet as f64)),
                ("ref", Json::Str(cell_name(c.row, c.col))),
                ("author", Json::Str(c.author.clone())),
                ("text", Json::Str(c.text.clone())),
            ])
        })
        .collect();
    Json::obj(vec![("comments", Json::Arr(comments))])
}

fn wb_export_csv(app: &App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let wb = &app.pkg.workbook;
    let csv = sheet_to_csv(&wb.sheets[si], &wb.styles, wb.date1904);
    Ok(Json::obj(vec![
        ("sheet", Json::Num(si as f64)),
        ("csv", Json::Str(csv)),
    ]))
}

/// One `{col, agg}` pair from a pivot verb's `values` array — shared by
/// `sheet.pivot` and `pivot.create`, `verb` names the caller in errors so
/// parity between the two stays honest about which verb actually failed.
fn parse_measure_arg(verb: &str, v: &Json) -> Result<(String, Agg), String> {
    let col = v
        .get_str("col")
        .ok_or_else(|| format!("{verb}: each value needs a 'col'"))?
        .to_string();
    let agg_s = v
        .get_str("agg")
        .ok_or_else(|| format!("{verb}: each value needs an 'agg'"))?;
    let agg = Agg::from_verb_name(agg_s).ok_or_else(|| format!("{verb}: unknown agg '{agg_s}'"))?;
    Ok((col, agg))
}

/// An array of header-name strings (`rows`/`cols`), defaulting to empty when
/// the key is absent.
fn names_arg(args: &Json, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Json::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Create a REAL, persistent workbook pivot — arg shape identical to the
/// ad-hoc `sheet.pivot` (same header-name resolution, same 11 agg strings,
/// same unknown-column error family), plus an optional `name`. Builds it via
/// [`gridcore::xlsx::SheetPackage::create_pivot`] (the TUI's own
/// `add_pivot` + field-layout machinery, given the full layout up front
/// instead of the interactive editor's one-field-at-a-time session) and
/// lands the output on a NEW sheet, mirroring the TUI's Ctrl-P placement.
///
/// Undo: clears history like `sheet.add`/`sheet.import-csv` — a new sheet +
/// pivot-part registration isn't a cell-level edit the undo stack can
/// invert. An agent-level inverse (MCP/wasm) must remove BOTH the created
/// sheet and the pivot registration; `SheetPackage::remove_sheet` already
/// cascades pivot removal for exactly this reason.
fn pivot_create(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let rg = args
        .get_str("range")
        .ok_or("pivot.create needs a 'range'")?;
    let (r1, c1, r2, c2) = parse_range(rg)?;
    let frame = Frame::from_range(&app.pkg.workbook, si, (r1, c1, r2, c2));
    if frame.names.is_empty() || frame.rows() == 0 {
        return Err("pivot.create: the range needs a header row and data rows".into());
    }

    // Same leniency note as sheet.pivot: the MCP schema marks `rows`
    // required; the code tolerates its absence (defaults to empty).
    let rows = names_arg(args, "rows");
    let cols = names_arg(args, "cols");
    let values_json = args
        .get("values")
        .and_then(Json::as_array)
        .ok_or("pivot.create needs a 'values' array")?;
    let values = values_json
        .iter()
        .map(|v| parse_measure_arg("pivot.create", v))
        .collect::<Result<Vec<_>, _>>()?;
    if values.is_empty() {
        return Err("pivot.create needs at least one value field".into());
    }
    let spec = pivot_spec_from_names(&frame, &rows, &cols, &values)
        .map_err(|col| format!("pivot.create: unknown column '{col}'"))?;

    let sheet_name = match args.get_str("name") {
        Some(n) => {
            if n.is_empty() || n.contains(['[', ']', '*', '?', ':', '/', '\\']) {
                return Err("invalid sheet name".into());
            }
            if app.pkg.workbook.sheet_index(n).is_some() {
                return Err(format!("pivot.create: sheet name '{n}' is already taken"));
            }
            n.to_string()
        }
        None => unique_pivot_name(&app.pkg.workbook),
    };

    let source = gridcore::pivot::PivotSource::Range {
        sheet: app.pkg.workbook.sheets[si].name.clone(),
        rect: (r1, c1, r2, c2),
    };
    let idx = app
        .pkg
        .create_pivot(source, &frame, &spec, &sheet_name)
        .ok_or("pivot.create: could not create the pivot")?;
    let dest = app.pkg.workbook.pivots[idx].sheet;

    // Same "can't be a cell-level undo" reasoning as sheet.add.
    app.undo.clear();
    app.redo.clear();
    app.rebuild_engine();
    app.modified = true;
    Ok(Json::obj(vec![
        ("sheet", Json::Num(dest as f64)),
        ("name", Json::Str(sheet_name)),
    ]))
}

/// A default pivot-sheet name: `Pivot1`, `Pivot2`, … — unique among existing
/// sheet names. Distinct pattern from `unique_sheet_name`'s "Sheet"/"Sheet 2"
/// (no space before the number) — Wave-3's convention for agent-created
/// pivot sheets, chosen for the verb's spec.
fn unique_pivot_name(wb: &gridcore::sheet::Workbook) -> String {
    let mut n = 1;
    loop {
        let candidate = format!("Pivot{n}");
        if wb.sheet_index(&candidate).is_none() {
            return candidate;
        }
        n += 1;
    }
}

/// Ad-hoc, read-only pivot over `range`: no workbook mutation, computed
/// straight from a [`Frame`] snapshot via [`gridcore::frame::pivot`].
fn sheet_pivot(app: &App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let rg = args.get_str("range").ok_or("sheet.pivot needs a 'range'")?;
    let (r1, c1, r2, c2) = parse_range(rg)?;
    let frame = Frame::from_range(&app.pkg.workbook, si, (r1, c1, r2, c2));

    // The MCP schema marks `rows` required; the code tolerates its absence
    // (defaults to empty). The schema is the contract — this leniency is
    // deliberate slack, not a documented behavior to rely on.
    let rows = names_arg(args, "rows");
    let cols = names_arg(args, "cols");
    let values_json = args
        .get("values")
        .and_then(Json::as_array)
        .ok_or("sheet.pivot needs a 'values' array")?;
    let values = values_json
        .iter()
        .map(|v| parse_measure_arg("sheet.pivot", v))
        .collect::<Result<Vec<_>, _>>()?;

    let spec = pivot_spec_from_names(&frame, &rows, &cols, &values)
        .map_err(|col| format!("sheet.pivot: unknown column '{col}'"))?;
    let out = pivot(&frame, &spec);
    let table = pivot_table_strings(&out)
        .into_iter()
        .map(|row| Json::Arr(row.into_iter().map(Json::Str).collect()))
        .collect();
    Ok(Json::obj(vec![("table", Json::Arr(table))]))
}

/// The typed result and general-format display text of a formula value.
fn formula_value_json(v: &Value) -> Json {
    match v {
        Value::Empty => Json::Null,
        Value::Num(n) => Json::Num(*n),
        Value::Str(s) => Json::Str(s.clone()),
        Value::Bool(b) => Json::Bool(*b),
        Value::Err(e) => Json::Str(e.code().to_string()),
    }
}

fn formula_value_text(v: &Value) -> String {
    match v {
        Value::Empty => String::new(),
        Value::Num(n) => fmt_general(*n),
        Value::Str(s) => s.clone(),
        Value::Bool(true) => "TRUE".to_string(),
        Value::Bool(false) => "FALSE".to_string(),
        Value::Err(e) => e.code().to_string(),
    }
}

/// Side-effect-free formula preview: evaluates `formula` against the live
/// workbook at `ref` (default A1) without writing anywhere.
fn formula_eval(app: &App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let formula = args
        .get_str("formula")
        .ok_or("formula.eval needs a 'formula'")?;
    let body = formula.strip_prefix('=').unwrap_or(formula);
    let (r, c) = match args.get_str("ref") {
        Some(rf) => parse_cell_name(rf.trim()).ok_or_else(|| format!("bad cell ref '{rf}'"))?,
        None => (0, 0),
    };
    let v = eval_formula_at(&app.pkg.workbook, si, r, c, body);
    Ok(Json::obj(vec![
        ("value", formula_value_json(&v)),
        ("text", Json::Str(formula_value_text(&v))),
    ]))
}

/// Summary statistics (sum/count/countNums/average/min/max) over `range`.
fn sheet_stats(app: &App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let rg = args.get_str("range").ok_or("sheet.stats needs a 'range'")?;
    let (r1, c1, r2, c2) = parse_range(rg)?;
    let s = &app.pkg.workbook.sheets[si];
    let mut vals = Vec::new();
    for r in r1..=r2 {
        for c in c1..=c2 {
            vals.push(
                s.cell(r, c)
                    .map(|cl| cell_to_value(&cl.value))
                    .unwrap_or(Value::Empty),
            );
        }
    }
    let st = range_stats(&vals);
    Ok(Json::obj(vec![
        ("sum", Json::Num(st.sum)),
        ("count", Json::Num(st.count as f64)),
        ("countNums", Json::Num(st.count_nums as f64)),
        ("average", Json::Num(st.average)),
        ("min", Json::Num(st.min)),
        ("max", Json::Num(st.max)),
    ]))
}

/// Every chart on the workbook, read from each sheet's already-parsed
/// `drawings` (populated at load time by `drawing::parse_drawings`) — the
/// same source the TUI's overlay reads to render chart boxes over the grid.
fn chart_list(app: &App) -> Json {
    let mut charts = Vec::new();
    for s in &app.pkg.workbook.sheets {
        for d in &s.drawings {
            let DrawingKind::Chart(cd) = &d.kind else {
                continue;
            };
            let mut fields = vec![("kind", Json::Str(cd.kind.clone()))];
            if !cd.title.is_empty() {
                fields.push(("title", Json::Str(cd.title.clone())));
            }
            fields.push((
                "categories",
                Json::Arr(cd.categories.iter().cloned().map(Json::Str).collect()),
            ));
            let series = cd
                .series
                .iter()
                .map(|ser| {
                    let mut sf = Vec::new();
                    if !ser.name.is_empty() {
                        sf.push(("name", Json::Str(ser.name.clone())));
                    }
                    sf.push((
                        "values",
                        Json::Arr(ser.values.iter().map(|v| Json::Num(*v)).collect()),
                    ));
                    Json::obj(sf)
                })
                .collect();
            fields.push(("series", Json::Arr(series)));
            charts.push(Json::obj(fields));
        }
    }
    Json::obj(vec![("charts", Json::Arr(charts))])
}

/// Every persistent pivot table, summarized: row/column field names and
/// value (data field) display names, from `workbook.pivots`.
fn pivot_list(app: &App) -> Json {
    let pivots = app
        .pkg
        .workbook
        .pivots
        .iter()
        .map(|p| {
            let field_name = |i: &usize| p.fields.get(*i).cloned().unwrap_or_default();
            let rows: Vec<Json> = p.row_fields.iter().map(field_name).map(Json::Str).collect();
            let cols: Vec<Json> = p.col_fields.iter().map(field_name).map(Json::Str).collect();
            let values: Vec<Json> = p
                .data_fields
                .iter()
                .map(|df| Json::Str(df.name.clone()))
                .collect();
            Json::obj(vec![
                ("sheet", Json::Num(p.sheet as f64)),
                ("rows", Json::Arr(rows)),
                ("cols", Json::Arr(cols)),
                ("values", Json::Arr(values)),
            ])
        })
        .collect();
    Json::obj(vec![("pivots", Json::Arr(pivots))])
}

// ---------------------------------------------------------------------------
// Mutating verbs (undoable, through the app's edit path)
// ---------------------------------------------------------------------------

fn cell_set(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let (r, c) = ref_arg(args)?;
    let text = args.get_str("text").ok_or("cell.set needs 'text'")?;
    // Same validation as the TUI's commit path: a bad formula is rejected
    // before it touches the workbook.
    if let Some(body) = text.strip_prefix('=') {
        if !body.is_empty() {
            Engine::validate(body).map_err(|e| format!("formula error: {e}"))?;
        }
    }
    let style = app.pkg.workbook.sheets[si]
        .cell(r, c)
        .map(|x| x.style)
        .unwrap_or(0);
    let mut cell = parse_input(text);
    cell.style = style;
    app.apply_on(si, vec![(r, c, cell)]);
    let s = &app.pkg.workbook.sheets[si];
    match s.cell(r, c) {
        Some(cell) => Ok(cell_json(r, c, cell)),
        None => Ok(Json::obj(vec![("ref", Json::Str(cell_name(r, c)))])),
    }
}

fn range_clear(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let rg = args.get_str("range").ok_or("range.clear needs a 'range'")?;
    let (r1, c1, r2, c2) = parse_range(rg)?;
    // Mirror the TUI's Delete: blank the value/formula but keep the style.
    let mut changes = Vec::new();
    for (&(r, c), cell) in app.pkg.workbook.sheets[si]
        .cells
        .range((r1, 0)..=(r2, u32::MAX))
    {
        if c < c1 || c > c2 || cell.is_blank() {
            continue;
        }
        changes.push((
            r,
            c,
            Cell {
                style: cell.style,
                ..Cell::default()
            },
        ));
    }
    let cleared = changes.len();
    app.apply_on(si, changes);
    Ok(Json::obj(vec![("cleared", Json::Num(cleared as f64))]))
}

/// Add a threaded comment (or a reply, if the cell already has a thread) —
/// mirrors the TUI's `commit_comment`. Comment data lives in package parts
/// outside the cell grid, so this is deliberately **not** pushed onto the
/// undo stack, exactly like the keyboard path.
fn comment_add(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let (r, c) = ref_arg(args)?;
    let text = args.get_str("text").ok_or("comment.add needs 'text'")?;
    if text.is_empty() {
        return Err("comment.add needs non-empty 'text'".into());
    }
    let author = args
        .get_str("author")
        .map(str::to_string)
        .unwrap_or_else(comment_author);
    app.pkg
        .add_threaded_comment(si, r, c, &author, text, &iso_now());
    app.modified = true;
    app.refresh_comments();
    Ok(Json::obj(vec![
        ("sheet", Json::Num(si as f64)),
        ("ref", Json::Str(cell_name(r, c))),
    ]))
}

/// Remove the comment (threaded or legacy note) on a cell, if any.
fn comment_remove(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let (r, c) = ref_arg(args)?;
    let existed = app
        .pkg
        .comments()
        .iter()
        .any(|cm| cm.sheet == si && cm.row == r && cm.col == c);
    if existed {
        app.pkg.remove_comment(si, r, c);
        app.modified = true;
        app.refresh_comments();
        // Gated no-op signal: only a real removal flashes the activity dot
        // (see the dispatch note above; matches docxy's no-op principle).
        ctlcore::signal_activity();
    }
    Ok(Json::obj(vec![("removed", Json::Bool(existed))]))
}

/// Write a rectangular block of cells starting at `start`, atomically: every
/// formula in the batch is validated *before* anything is applied, so a bad
/// formula anywhere in the block leaves the sheet (and the undo stack)
/// completely untouched. The whole block lands as one [`App::apply_on`]
/// call, i.e. one undo group.
fn range_set(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let start = args.get_str("start").ok_or("range.set needs a 'start'")?;
    let (r0, c0) =
        parse_cell_name(start.trim()).ok_or_else(|| format!("bad cell ref '{start}'"))?;
    let rows_json = args
        .get("rows")
        .and_then(Json::as_array)
        .ok_or("range.set needs a 'rows' array")?;

    let mut entries = Vec::new();
    for (i, row) in rows_json.iter().enumerate() {
        let row_arr = row
            .as_array()
            .ok_or("range.set: each row must be an array of strings")?;
        for (j, cellv) in row_arr.iter().enumerate() {
            let text = cellv
                .as_str()
                .ok_or("range.set: each cell must be a string")?;
            entries.push((r0 + i as u32, c0 + j as u32, text.to_string()));
        }
    }

    // Pass 1: validate every formula before touching anything (atomicity).
    for (r, c, text) in &entries {
        if let Some(body) = text.strip_prefix('=') {
            if !body.is_empty() {
                Engine::validate(body).map_err(|e| {
                    format!("range.set: formula error at {}: {e}", cell_name(*r, *c))
                })?;
            }
        }
    }

    // Pass 2: every entry validated — build the changes and apply as one group.
    let sheet = &app.pkg.workbook.sheets[si];
    let changes: Vec<(u32, u32, Cell)> = entries
        .into_iter()
        .map(|(r, c, text)| {
            let style = sheet.cell(r, c).map(|x| x.style).unwrap_or(0);
            let mut cell = parse_input(&text);
            cell.style = style;
            (r, c, cell)
        })
        .collect();
    let n = changes.len();
    app.apply_on(si, changes);
    Ok(Json::obj(vec![("set", Json::Num(n as f64))]))
}

/// A sheet name derived from `base`, deduplicated against existing sheet
/// names by appending " 2", " 3", … (the same scheme `create_pivot_from`/
/// `build_model_report` use for their generated sheets).
fn unique_sheet_name(wb: &gridcore::sheet::Workbook, base: &str) -> String {
    if wb.sheet_index(base).is_none() {
        return base.to_string();
    }
    let mut n = 1;
    loop {
        n += 1;
        let candidate = format!("{base} {n}");
        if wb.sheet_index(&candidate).is_none() {
            return candidate;
        }
    }
}

/// Import CSV text as a brand-new sheet (never overwrites an existing one —
/// name collisions are deduplicated). Mirrors the shape of the TUI's
/// `csv_to_pkg`, but populates a sheet inside the *live* workbook instead of
/// building a standalone package.
fn sheet_import_csv(app: &mut App, args: &Json) -> Result<Json, String> {
    let text = args
        .get_str("text")
        .ok_or("sheet.import-csv needs 'text'")?;
    let frame = Frame::from_csv(text);
    let requested = args.get_str("name").unwrap_or("Sheet");
    let name = unique_sheet_name(&app.pkg.workbook, requested);
    let idx = app.pkg.add_sheet(&name);
    frame.write_to_sheet(&mut app.pkg.workbook.sheets[idx]);
    let (rows, cols) = app.pkg.workbook.sheets[idx].used_size();
    // New package parts (worksheet/relationship/workbook.xml wiring) don't
    // fit the cell-level undo model — same as the TUI's own AddSheet flow,
    // which clears history rather than push an entry it couldn't invert.
    app.undo.clear();
    app.redo.clear();
    app.rebuild_engine();
    app.modified = true;
    Ok(Json::obj(vec![
        ("sheet", Json::Num(idx as f64)),
        ("name", Json::Str(name)),
        ("rows", Json::Num(rows as f64)),
        ("cols", Json::Num(cols as f64)),
    ]))
}

/// Literal find/replace across every cell's input text, on **every sheet** —
/// the workbook-wide counterpart of the TUI's per-sheet `replace_all`. Runs
/// through [`App::structural`] (not `apply_on`) so the whole multi-sheet
/// edit lands as a single undo group; `structural` already rebuilds the
/// engine and recalculates afterward.
fn wb_replace_all(app: &mut App, args: &Json) -> Result<Json, String> {
    let query = args
        .get_str("query")
        .ok_or("wb.replace-all needs a 'query'")?;
    if query.is_empty() {
        return Err("empty query".into());
    }
    let text = args.get_str("text").ok_or("wb.replace-all needs 'text'")?;
    let mut replaced = 0usize;
    app.structural(|wb| {
        for sheet in &mut wb.sheets {
            let changes = gridcore::edit::replace_all_in_sheet(sheet, query, text);
            replaced += changes.len();
            for (r, c, nc) in changes {
                sheet.set_cell(r, c, nc);
            }
        }
    });
    Ok(Json::obj(vec![("replaced", Json::Num(replaced as f64))]))
}

/// Add a new sheet (default base name "Sheet", deduplicated on collision —
/// never errors on a taken name).
fn sheet_add(app: &mut App, args: &Json) -> Result<Json, String> {
    let requested = args.get_str("name").unwrap_or("Sheet");
    let name = unique_sheet_name(&app.pkg.workbook, requested);
    let idx = app.pkg.add_sheet(&name);
    // Same "can't be a cell-level undo" reasoning as sheet.import-csv.
    app.undo.clear();
    app.redo.clear();
    app.rebuild_engine();
    app.modified = true;
    Ok(Json::obj(vec![
        ("sheet", Json::Num(idx as f64)),
        ("name", Json::Str(name)),
    ]))
}

/// Remove a sheet (errors on the last one — a workbook must keep at least
/// one). `sheet` is required here, unlike the other verbs' `sheet?`: a
/// destructive op shouldn't silently default to "whichever sheet is active".
fn sheet_remove(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg_required(app, args)?;
    let removed_active = app.sheet == si;
    if !app.pkg.remove_sheet(si) {
        return Err("cannot remove the last sheet".into());
    }
    // Indices above the removed sheet shift down by one; an unaffected
    // sheet below it keeps its index untouched. Only reset the viewport
    // when the ACTIVE sheet itself is the one that just disappeared —
    // mirrors the TUI's `delete_current_sheet`, which only ever removes
    // the active sheet and so always resets. Removing some other sheet
    // must leave a human's cursor/viewport on the sheet they're looking at
    // exactly as they left it.
    if app.sheet > si {
        app.sheet -= 1;
    } else if removed_active {
        app.sheet = app.sheet.min(app.pkg.workbook.sheets.len() - 1);
    }
    if removed_active {
        app.cur = (0, 0);
        app.top = 0;
        app.left = 0;
        app.anchor = None;
    }
    // Same "can't be a cell-level undo" reasoning as sheet.import-csv.
    app.undo.clear();
    app.redo.clear();
    app.rebuild_engine();
    app.modified = true;
    Ok(Json::obj(vec![("removed", Json::Bool(true))]))
}

/// Rename a sheet and rewrite every formula/defined-name reference to it —
/// via [`App::structural`], so it's one undo group like the TUI's own
/// RenameSheet prompt.
fn sheet_rename(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg_required(app, args)?;
    let name = args.get_str("name").ok_or("sheet.rename needs a 'name'")?;
    if name.is_empty() || name.contains(['[', ']', '*', '?', ':', '/', '\\']) {
        return Err("invalid sheet name".into());
    }
    let name = name.to_string();
    let new_name = name.clone();
    app.structural(move |wb| gridcore::edit::rename_sheet(wb, si, &new_name));
    Ok(Json::obj(vec![("name", Json::Str(name))]))
}

/// Insert or delete `count` rows at 0-based row `at` — via [`App::structural`],
/// mirroring the TUI's Ctrl-+/Ctrl-- row operations.
fn row_op(app: &mut App, args: &Json, insert: bool) -> Result<Json, String> {
    let verb = if insert { "row.insert" } else { "row.delete" };
    let si = sheet_arg(app, args)?;
    let at = args
        .get_usize("at")
        .ok_or_else(|| format!("{verb} needs an 'at'"))? as u32;
    let count = args.get_usize("count").unwrap_or(1) as u32;
    if count == 0 {
        return Err(format!("{verb}: 'count' must be at least 1"));
    }
    app.structural(|wb| {
        if insert {
            gridcore::edit::insert_rows(wb, si, at, count);
        } else {
            gridcore::edit::delete_rows(wb, si, at, count);
        }
    });
    let key = if insert { "inserted" } else { "deleted" };
    Ok(Json::obj(vec![(key, Json::Num(count as f64))]))
}

/// Insert or delete `count` columns at 0-based column `at` — via
/// [`App::structural`], mirroring the TUI's column operations.
fn col_op(app: &mut App, args: &Json, insert: bool) -> Result<Json, String> {
    let verb = if insert { "col.insert" } else { "col.delete" };
    let si = sheet_arg(app, args)?;
    let at = args
        .get_usize("at")
        .ok_or_else(|| format!("{verb} needs an 'at'"))? as u32;
    let count = args.get_usize("count").unwrap_or(1) as u32;
    if count == 0 {
        return Err(format!("{verb}: 'count' must be at least 1"));
    }
    app.structural(|wb| {
        if insert {
            gridcore::edit::insert_cols(wb, si, at, count);
        } else {
            gridcore::edit::delete_cols(wb, si, at, count);
        }
    });
    let key = if insert { "inserted" } else { "deleted" };
    Ok(Json::obj(vec![(key, Json::Num(count as f64))]))
}

/// Build `gridcore::format::FormatPatch`'s wire pairs from the `patch`
/// object's own JSON values — gridcore stays JSON-free, so scalars are
/// stringified here (`true`/`false` for booleans, the raw text for
/// strings) and [`FormatPatch::parse`] does the actual key/value
/// validation. Key order is preserved from the request.
fn patch_pairs(patch: &Json) -> Result<Vec<(String, String)>, String> {
    let Json::Obj(pairs) = patch else {
        return Err("cell.format needs a 'patch' object".to_string());
    };
    Ok(pairs
        .iter()
        .map(|(k, v)| {
            let text = match v {
                Json::Str(s) => s.clone(),
                Json::Bool(b) => b.to_string(),
                Json::Num(n) => n.to_string(),
                Json::Null | Json::Arr(_) | Json::Obj(_) => String::new(),
            };
            (k.clone(), text)
        })
        .collect())
}

/// Set `patch` over every cell in `range`, on the existing
/// `Styles::intern`/`apply_format` path — one [`App::apply_on`] call, so the
/// whole range lands as ONE undo group exactly like the TUI's own
/// `apply_format`. Value/formula/spill are preserved; only each cell's style
/// index changes.
fn cell_format(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let rg = args.get_str("range").ok_or("cell.format needs a 'range'")?;
    let (r1, c1, r2, c2) = parse_range(rg)?;
    // Reject an oversized range BEFORE materializing a Cell per coordinate or
    // touching the undo stack — see `gridcore::format::check_format_range_cap`.
    gridcore::format::check_format_range_cap(r1, c1, r2, c2)?;
    let patch_arg = args.get("patch").ok_or("cell.format needs a 'patch'")?;
    let pairs = patch_pairs(patch_arg)?;
    let patch = FormatPatch::parse(&pairs)?;

    let snapshot: Vec<(u32, u32, Option<Cell>)> = {
        let sheet = &app.pkg.workbook.sheets[si];
        let mut v = Vec::new();
        for r in r1..=r2 {
            for c in c1..=c2 {
                v.push((r, c, sheet.cell(r, c).cloned()));
            }
        }
        v
    };
    let mut changes = Vec::with_capacity(snapshot.len());
    for (r, c, existing) in snapshot {
        let cur = existing.as_ref().map(|cl| cl.style).unwrap_or(0);
        let base_xf = app.pkg.workbook.styles.xf(cur);
        let new_xf = apply_patch_to_xf(&base_xf, &patch);
        let idx = app.pkg.workbook.styles.intern(new_xf);
        let mut cell = existing.unwrap_or_default();
        cell.style = idx;
        changes.push((r, c, cell));
    }
    let formatted = changes.len();
    app.apply_on(si, changes);
    Ok(Json::obj(vec![("formatted", Json::Num(formatted as f64))]))
}

/// Resolve the `col` arg — a column letter (`"C"`), a 0-based numeric index,
/// or a digit-string index (`"5"`, so the schema's "letter or 0-based index"
/// description is truthful for schema-conforming string inputs too) —
/// mirroring [`sheet_arg`]'s index-or-name flexibility. Every arm is bound to
/// `gridcore::sheet::MAX_COLS`, the same bound [`parse_col`] already applies
/// to the letter arm: an out-of-range numeric index used to sail straight
/// through into a saved `.xlsx`'s `ColDef`, which Excel then refuses to open
/// without a "needs repair" prompt.
fn col_arg(args: &Json) -> Result<u32, String> {
    match args.get("col") {
        Some(Json::Num(_)) => {
            let c = args
                .get_usize("col")
                .map(|c| c as u32)
                .ok_or_else(|| "bad 'col' index".to_string())?;
            if c >= gridcore::sheet::MAX_COLS {
                return Err(format!("bad column '{c}'"));
            }
            Ok(c)
        }
        Some(Json::Str(s)) => {
            let t = s.trim();
            if let Some((col, used)) = parse_col(t) {
                if used == t.len() {
                    return Ok(col);
                }
            } else if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
                if let Ok(c) = t.parse::<u32>() {
                    if c < gridcore::sheet::MAX_COLS {
                        return Ok(c);
                    }
                }
            }
            Err(format!("bad column '{s}'"))
        }
        _ => Err("col.width needs a 'col' (letter or 0-based index)".to_string()),
    }
}

/// Set one column's display width — directly, like the TUI's own F7/F8
/// width-adjust keys, which mutate `Sheet::set_col_width` without pushing
/// onto the undo stack (empirically: no `self.undo.push` on that path in
/// `main.rs`). This verb mirrors that: NOT on the undo/redo stack.
fn col_width(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let col = col_arg(args)?;
    let width = args
        .get("width")
        .and_then(Json::as_f64)
        .ok_or("col.width needs a 'width' number")?;
    if !(width.is_finite() && width > 0.0) {
        return Err("col.width: 'width' must be positive".to_string());
    }
    app.pkg.workbook.sheets[si].set_col_width(col, width);
    app.modified = true;
    Ok(Json::obj(vec![
        ("col", Json::Num(col as f64)),
        ("width", Json::Num(width)),
    ]))
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

/// Resolve the `sheet` arg (index or name) to a sheet index; default = active.
fn sheet_arg(app: &App, args: &Json) -> Result<usize, String> {
    let wb = &app.pkg.workbook;
    match args.get("sheet") {
        None | Some(Json::Null) => Ok(app.sheet),
        Some(Json::Num(_)) => {
            let i = args.get_usize("sheet").ok_or("bad sheet index")?;
            if i < wb.sheets.len() {
                Ok(i)
            } else {
                Err(format!(
                    "sheet {i} out of bounds ({} sheets)",
                    wb.sheets.len()
                ))
            }
        }
        Some(Json::Str(name)) => wb
            .sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("no sheet named '{name}'")),
        Some(_) => Err("'sheet' must be an index or a name".into()),
    }
}

/// Like [`sheet_arg`], but the `sheet` key must be present — for ops
/// (rename/remove) that would be dangerous applied to the wrong sheet by a
/// silent default to "whichever one is active".
fn sheet_arg_required(app: &App, args: &Json) -> Result<usize, String> {
    if matches!(args.get("sheet"), None | Some(Json::Null)) {
        return Err("needs a 'sheet' (index or name)".into());
    }
    sheet_arg(app, args)
}

/// Parse the `ref` arg (`"B4"`) into (row, col).
fn ref_arg(args: &Json) -> Result<(u32, u32), String> {
    let r = args
        .get_str("ref")
        .ok_or("needs a cell 'ref' like \"B4\"")?;
    parse_cell_name(r.trim()).ok_or_else(|| format!("bad cell ref '{r}'"))
}

/// Parse `"A1:C10"` (or a single `"B4"`) into (r1, c1, r2, c2), normalized.
fn parse_range(s: &str) -> Result<(u32, u32, u32, u32), String> {
    let t = s.trim();
    if let Some((r1, c1, r2, c2)) = parse_range_name(t) {
        return Ok((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)));
    }
    if let Some((r, c)) = parse_cell_name(t) {
        return Ok((r, c, r, c));
    }
    Err(format!("bad range '{s}' (use A1 or A1:C10)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridcore::xlsx::new_xlsx;

    fn app() -> App {
        let mut a = App::new(new_xlsx(), "ctl-test.xlsx");
        a.os_clip = None;
        a
    }

    fn set(app: &mut App, r: &str, text: &str) {
        cell_set(
            app,
            &Json::obj(vec![
                ("ref", Json::Str(r.into())),
                ("text", Json::Str(text.into())),
            ]),
        )
        .unwrap();
    }

    #[test]
    fn path_reports_workbook_shape() {
        let a = app();
        let r = path_info(&a);
        assert_eq!(r.get_str("path"), Some("ctl-test.xlsx"));
        assert_eq!(r.get("modified").unwrap().as_bool(), Some(false));
        assert_eq!(r.get_usize("active"), Some(0));
        assert!(r.get_usize("sheets").unwrap() >= 1);
    }

    #[test]
    fn set_and_get_a_value_and_a_formula() {
        let mut a = app();
        set(&mut a, "A1", "10");
        set(&mut a, "A2", "20");
        set(&mut a, "A3", "=SUM(A1:A2)");
        assert!(a.modified);
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A3".into()))])).unwrap();
        assert_eq!(g.get("value").unwrap().as_f64(), Some(30.0));
        assert_eq!(g.get_str("formula"), Some("=SUM(A1:A2)"));
        assert_eq!(g.get_str("text"), Some("30"));
    }

    #[test]
    fn edits_recalculate_dependents() {
        let mut a = app();
        set(&mut a, "B1", "5");
        set(&mut a, "B2", "=B1*3");
        set(&mut a, "B1", "7");
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("B2".into()))])).unwrap();
        assert_eq!(g.get("value").unwrap().as_f64(), Some(21.0));
    }

    #[test]
    fn bad_formula_is_rejected_without_touching_the_sheet() {
        let mut a = app();
        let err = cell_set(
            &mut a,
            &Json::obj(vec![
                ("ref", Json::Str("A1".into())),
                ("text", Json::Str("=SUM((".into())),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("formula error"));
        assert!(!a.modified);
    }

    #[test]
    fn sheet_read_returns_window_and_respects_range() {
        let mut a = app();
        set(&mut a, "A1", "1");
        set(&mut a, "B2", "two");
        set(&mut a, "C3", "=A1+1");
        let all = sheet_read(&a, &Json::Null).unwrap();
        assert_eq!(all.get("cells").unwrap().as_array().unwrap().len(), 3);
        assert_eq!(all.get("truncated").unwrap().as_bool(), Some(false));
        let window =
            sheet_read(&a, &Json::obj(vec![("range", Json::Str("A1:B2".into()))])).unwrap();
        let cells = window.get("cells").unwrap().as_array().unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].get_str("ref"), Some("A1"));
        assert_eq!(cells[1].get_str("ref"), Some("B2"));
    }

    #[test]
    fn range_clear_blanks_cells_and_is_undoable() {
        let mut a = app();
        set(&mut a, "A1", "1");
        set(&mut a, "A2", "2");
        let r = range_clear(
            &mut a,
            &Json::obj(vec![("range", Json::Str("A1:A2".into()))]),
        )
        .unwrap();
        assert_eq!(r.get_usize("cleared"), Some(2));
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get("value"), Some(&Json::Null));
        // One undo restores the whole clear as a single group.
        a.undo();
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get("value").unwrap().as_f64(), Some(1.0));
    }

    #[test]
    fn agent_edits_share_the_undo_stack() {
        let mut a = app();
        set(&mut a, "A1", "42");
        a.undo();
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get("value"), Some(&Json::Null));
        a.redo();
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get("value").unwrap().as_f64(), Some(42.0));
    }

    #[test]
    fn find_scans_values_and_formulas() {
        let mut a = app();
        set(&mut a, "A1", "hello world");
        set(&mut a, "B1", "=SUM(1,2)");
        let r = find(&a, &Json::obj(vec![("query", Json::Str("world".into()))])).unwrap();
        assert_eq!(r.get_usize("count"), Some(1));
        let r = find(&a, &Json::obj(vec![("query", Json::Str("sum".into()))])).unwrap();
        assert_eq!(r.get_usize("count"), Some(1));
        let m = &r.get("matches").unwrap().as_array().unwrap()[0];
        assert_eq!(m.get_str("ref"), Some("B1"));
        assert_eq!(m.get_usize("sheet"), Some(0));
    }

    #[test]
    fn sheet_arg_accepts_index_and_name() {
        let a = app();
        assert_eq!(sheet_arg(&a, &Json::Null).unwrap(), 0);
        assert_eq!(
            sheet_arg(&a, &Json::obj(vec![("sheet", Json::Num(0.0))])).unwrap(),
            0
        );
        let name = a.pkg.workbook.sheets[0].name.clone();
        assert_eq!(
            sheet_arg(&a, &Json::obj(vec![("sheet", Json::Str(name))])).unwrap(),
            0
        );
        assert!(sheet_arg(&a, &Json::obj(vec![("sheet", Json::Num(9.0))])).is_err());
        assert!(sheet_arg(&a, &Json::obj(vec![("sheet", Json::Str("nope".into()))])).is_err());
    }

    #[test]
    fn dispatch_routes_and_reports_unknown() {
        let mut a = app();
        assert!(dispatch(&mut a, "wb.path", &Json::Null).is_ok());
        assert!(dispatch(&mut a, "sheet.list", &Json::Null).is_ok());
        let err = dispatch(&mut a, "wb.frobnicate", &Json::Null).unwrap_err();
        assert!(err.contains("unknown verb"));
    }

    #[test]
    fn parse_range_forms() {
        assert_eq!(parse_range("A1:C10").unwrap(), (0, 0, 9, 2));
        assert_eq!(parse_range("B4").unwrap(), (3, 1, 3, 1));
        // Reversed corners normalize.
        assert_eq!(parse_range("C10:A1").unwrap(), (0, 0, 9, 2));
        assert!(parse_range("junk!").is_err());
    }

    // -----------------------------------------------------------------
    // Wave-1 read verbs
    // -----------------------------------------------------------------

    #[test]
    fn comment_list_flattens_comments_in_reply_order() {
        let mut a = app();
        a.pkg.set_comment(0, 1, 2, "Reviewer", "Check this value");
        a.pkg
            .add_threaded_comment(0, 3, 0, "Ana", "A note", "2024-01-02T03:04:05Z");
        let r = dispatch(&mut a, "comment.list", &Json::Null).unwrap();
        let comments = r.get("comments").unwrap().as_array().unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].get_usize("sheet"), Some(0));
        assert_eq!(comments[0].get_str("ref"), Some("C2"));
        assert_eq!(comments[0].get_str("author"), Some("Reviewer"));
        assert_eq!(comments[0].get_str("text"), Some("Check this value"));
        assert_eq!(comments[1].get_str("ref"), Some("A4"));
        assert_eq!(comments[1].get_str("author"), Some("Ana"));
    }

    #[test]
    fn comment_list_empty_on_plain_fixture() {
        let mut a = app();
        let r = dispatch(&mut a, "comment.list", &Json::Null).unwrap();
        assert_eq!(r.get("comments").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn wb_export_csv_returns_display_formatted_text() {
        let mut a = app();
        set(&mut a, "A1", "name");
        set(&mut a, "B1", "amount");
        set(&mut a, "A2", "Alice");
        set(&mut a, "B2", "30");
        let r = dispatch(&mut a, "wb.export-csv", &Json::Null).unwrap();
        assert_eq!(r.get_usize("sheet"), Some(0));
        assert_eq!(r.get_str("csv"), Some("name,amount\nAlice,30\n"));
    }

    fn pivot_fixture(a: &mut App) {
        set(a, "A1", "name");
        set(a, "B1", "amount");
        set(a, "A2", "Alice");
        set(a, "B2", "10");
        set(a, "A3", "Bob");
        set(a, "B3", "20");
        set(a, "A4", "Alice");
        set(a, "B4", "20");
    }

    #[test]
    fn sheet_pivot_sums_by_group_including_header_row() {
        let mut a = app();
        pivot_fixture(&mut a);
        let r = dispatch(
            &mut a,
            "sheet.pivot",
            &Json::obj(vec![
                ("range", Json::Str("A1:B4".into())),
                ("rows", Json::Arr(vec![Json::Str("name".into())])),
                (
                    "values",
                    Json::Arr(vec![Json::obj(vec![
                        ("col", Json::Str("amount".into())),
                        ("agg", Json::Str("sum".into())),
                    ])]),
                ),
            ]),
        )
        .unwrap();
        let table = r.get("table").unwrap().as_array().unwrap();
        let row_strs: Vec<Vec<&str>> = table
            .iter()
            .map(|row| {
                row.as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c.as_str().unwrap())
                    .collect()
            })
            .collect();
        assert_eq!(row_strs[0], vec!["name", "Sum of amount"]);
        assert_eq!(row_strs[1], vec!["Alice", "30"]);
        assert_eq!(row_strs[2], vec!["Bob", "20"]);
    }

    #[test]
    fn sheet_pivot_unknown_header_names_the_column() {
        let mut a = app();
        pivot_fixture(&mut a);
        let err = dispatch(
            &mut a,
            "sheet.pivot",
            &Json::obj(vec![
                ("range", Json::Str("A1:B4".into())),
                ("rows", Json::Arr(vec![Json::Str("nope".into())])),
                ("values", Json::Arr(vec![])),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("nope"), "error should name the column: {err}");
    }

    #[test]
    fn formula_eval_returns_value_and_text_without_mutating() {
        let mut a = app();
        set(&mut a, "A1", "10");
        let modified_before = a.modified;
        let r = dispatch(
            &mut a,
            "formula.eval",
            &Json::obj(vec![
                ("formula", Json::Str("=A1+1".into())),
                ("ref", Json::Str("B5".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get("value").unwrap().as_f64(), Some(11.0));
        assert_eq!(r.get_str("text"), Some("11"));
        // Nothing was written at the context ref or anywhere else.
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("B5".into()))])).unwrap();
        assert_eq!(g.get("value"), Some(&Json::Null));
        // formula.eval itself flips nothing beyond the prior cell.set.
        assert_eq!(a.modified, modified_before);
    }

    #[test]
    fn formula_eval_defaults_ref_to_a1_and_reports_errors() {
        let mut a = app();
        let r = dispatch(
            &mut a,
            "formula.eval",
            &Json::obj(vec![("formula", Json::Str("=1/0".into()))]),
        )
        .unwrap();
        assert_eq!(r.get_str("text"), Some("#DIV/0!"));
    }

    #[test]
    fn sheet_stats_returns_all_six_keys_over_numeric_range() {
        let mut a = app();
        set(&mut a, "A1", "10");
        set(&mut a, "A2", "20");
        set(&mut a, "A3", "-5");
        let r = dispatch(
            &mut a,
            "sheet.stats",
            &Json::obj(vec![("range", Json::Str("A1:A3".into()))]),
        )
        .unwrap();
        assert_eq!(r.get("sum").unwrap().as_f64(), Some(25.0));
        assert_eq!(r.get_usize("count"), Some(3));
        assert_eq!(r.get_usize("countNums"), Some(3));
        assert_eq!(r.get("average").unwrap().as_f64(), Some(25.0 / 3.0));
        assert_eq!(r.get("min").unwrap().as_f64(), Some(-5.0));
        assert_eq!(r.get("max").unwrap().as_f64(), Some(20.0));
    }

    #[test]
    fn chart_list_empty_on_plain_fixture() {
        let mut a = app();
        let r = dispatch(&mut a, "chart.list", &Json::Null).unwrap();
        assert_eq!(r.get("charts").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn chart_list_reports_kind_title_categories_and_series() {
        use gridcore::sheet::{ChartData, ChartSeries, Drawing, DrawingKind};
        let mut a = app();
        a.pkg.workbook.sheets[0].drawings.push(Drawing {
            from: (0, 0),
            to: (10, 5),
            kind: DrawingKind::Chart(ChartData {
                title: "Sales".into(),
                kind: "bar".into(),
                categories: vec!["North".into(), "South".into()],
                series: vec![ChartSeries {
                    name: "Q1".into(),
                    values: vec![10.0, 20.0],
                }],
            }),
        });
        let r = dispatch(&mut a, "chart.list", &Json::Null).unwrap();
        let charts = r.get("charts").unwrap().as_array().unwrap();
        assert_eq!(charts.len(), 1);
        assert_eq!(charts[0].get_str("kind"), Some("bar"));
        assert_eq!(charts[0].get_str("title"), Some("Sales"));
        let cats = charts[0].get("categories").unwrap().as_array().unwrap();
        assert_eq!(cats[0].as_str(), Some("North"));
        let series = charts[0].get("series").unwrap().as_array().unwrap();
        assert_eq!(series[0].get_str("name"), Some("Q1"));
        let vals = series[0].get("values").unwrap().as_array().unwrap();
        assert_eq!(vals[0].as_f64(), Some(10.0));
    }

    #[test]
    fn pivot_list_empty_on_plain_fixture() {
        let mut a = app();
        let r = dispatch(&mut a, "pivot.list", &Json::Null).unwrap();
        assert_eq!(r.get("pivots").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn pivot_list_summarizes_rows_cols_and_values() {
        use gridcore::pivot::{DataField, Pivot, PivotSource};
        let mut a = app();
        a.pkg.workbook.pivots.push(Pivot {
            name: "P".into(),
            sheet: 0,
            location: (0, 3, 0, 3),
            source: PivotSource::Range {
                sheet: "Sheet1".into(),
                rect: (0, 0, 3, 1),
            },
            fields: vec!["Region".into(), "Sales".into()],
            row_fields: vec![0],
            col_fields: vec![],
            data_fields: vec![DataField {
                name: "Sum of Sales".into(),
                field: 1,
                agg: gridcore::frame::Agg::Sum,
            }],
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
            edited: false,
            part: String::new(),
            cache_part: String::new(),
        });
        let r = dispatch(&mut a, "pivot.list", &Json::Null).unwrap();
        let pivots = r.get("pivots").unwrap().as_array().unwrap();
        assert_eq!(pivots.len(), 1);
        assert_eq!(pivots[0].get_usize("sheet"), Some(0));
        let rows = pivots[0].get("rows").unwrap().as_array().unwrap();
        assert_eq!(rows[0].as_str(), Some("Region"));
        assert_eq!(pivots[0].get("cols").unwrap().as_array().unwrap().len(), 0);
        let values = pivots[0].get("values").unwrap().as_array().unwrap();
        assert_eq!(values[0].as_str(), Some("Sum of Sales"));
    }

    // -----------------------------------------------------------------
    // Wave-1 mutating verbs
    // -----------------------------------------------------------------

    #[test]
    fn comment_add_returns_sheet_and_ref_and_is_visible_in_list() {
        let mut a = app();
        let r = dispatch(
            &mut a,
            "comment.add",
            &Json::obj(vec![
                ("ref", Json::Str("B2".into())),
                ("text", Json::Str("Check this".into())),
                ("author", Json::Str("Ana".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("sheet"), Some(0));
        assert_eq!(r.get_str("ref"), Some("B2"));
        let list = dispatch(&mut a, "comment.list", &Json::Null).unwrap();
        let comments = list.get("comments").unwrap().as_array().unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].get_str("author"), Some("Ana"));
        assert_eq!(comments[0].get_str("text"), Some("Check this"));
    }

    #[test]
    fn comment_add_defaults_author_when_omitted() {
        let mut a = app();
        dispatch(
            &mut a,
            "comment.add",
            &Json::obj(vec![
                ("ref", Json::Str("A1".into())),
                ("text", Json::Str("Hi".into())),
            ]),
        )
        .unwrap();
        let list = dispatch(&mut a, "comment.list", &Json::Null).unwrap();
        let comments = list.get("comments").unwrap().as_array().unwrap();
        assert!(!comments[0].get_str("author").unwrap().is_empty());
    }

    #[test]
    fn comment_add_rejects_empty_text() {
        let mut a = app();
        let err = dispatch(
            &mut a,
            "comment.add",
            &Json::obj(vec![
                ("ref", Json::Str("A1".into())),
                ("text", Json::Str("".into())),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("text"));
    }

    #[test]
    fn comment_remove_reports_removed_bool() {
        let mut a = app();
        dispatch(
            &mut a,
            "comment.add",
            &Json::obj(vec![
                ("ref", Json::Str("A1".into())),
                ("text", Json::Str("Hi".into())),
            ]),
        )
        .unwrap();
        let r = dispatch(
            &mut a,
            "comment.remove",
            &Json::obj(vec![("ref", Json::Str("A1".into()))]),
        )
        .unwrap();
        assert_eq!(r.get("removed").unwrap().as_bool(), Some(true));
        // Already gone: reports false, doesn't error.
        let r = dispatch(
            &mut a,
            "comment.remove",
            &Json::obj(vec![("ref", Json::Str("A1".into()))]),
        )
        .unwrap();
        assert_eq!(r.get("removed").unwrap().as_bool(), Some(false));
    }

    #[test]
    fn comment_remove_noop_does_not_mark_modified() {
        // A no-op comment.remove (nothing on the cell) must not look like an
        // edit: it neither marks the workbook modified nor flashes the activity
        // dot — both ride the same `existed` guard (docxy's no-op principle).
        let mut a = app();
        assert!(!a.modified, "a fresh app starts unmodified");
        let r = dispatch(
            &mut a,
            "comment.remove",
            &Json::obj(vec![("ref", Json::Str("A1".into()))]),
        )
        .unwrap();
        assert_eq!(r.get("removed").unwrap().as_bool(), Some(false));
        assert!(!a.modified, "a no-op comment.remove must not mark modified");
    }

    #[test]
    fn comments_are_not_on_the_undo_stack() {
        let mut a = app();
        set(&mut a, "A1", "1"); // one undoable edit on the stack
        dispatch(
            &mut a,
            "comment.add",
            &Json::obj(vec![
                ("ref", Json::Str("A1".into())),
                ("text", Json::Str("Hi".into())),
            ]),
        )
        .unwrap();
        // A single undo() restores A1's value; the comment op never touched
        // the stack at all, so the comment survives untouched.
        a.undo();
        let list = dispatch(&mut a, "comment.list", &Json::Null).unwrap();
        assert_eq!(list.get("comments").unwrap().as_array().unwrap().len(), 1);
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get("value"), Some(&Json::Null));
    }

    #[test]
    fn range_set_writes_a_block_and_reports_count() {
        let mut a = app();
        let r = dispatch(
            &mut a,
            "range.set",
            &Json::obj(vec![
                ("start", Json::Str("A1".into())),
                (
                    "rows",
                    Json::Arr(vec![
                        Json::Arr(vec![Json::Str("1".into()), Json::Str("2".into())]),
                        Json::Arr(vec![Json::Str("3".into()), Json::Str("=A1+B1".into())]),
                    ]),
                ),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("set"), Some(4));
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("B2".into()))])).unwrap();
        assert_eq!(g.get("value").unwrap().as_f64(), Some(3.0)); // A1(1)+B1(2)
    }

    #[test]
    fn range_set_empty_string_clears_a_cell() {
        let mut a = app();
        set(&mut a, "A1", "old");
        dispatch(
            &mut a,
            "range.set",
            &Json::obj(vec![
                ("start", Json::Str("A1".into())),
                (
                    "rows",
                    Json::Arr(vec![Json::Arr(vec![Json::Str("".into())])]),
                ),
            ]),
        )
        .unwrap();
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get("value"), Some(&Json::Null));
    }

    #[test]
    fn range_set_is_atomic_bad_formula_touches_nothing() {
        let mut a = app();
        set(&mut a, "A1", "keep-me");
        let undo_depth_before = a.undo.len();
        let err = dispatch(
            &mut a,
            "range.set",
            &Json::obj(vec![
                ("start", Json::Str("A1".into())),
                (
                    "rows",
                    Json::Arr(vec![Json::Arr(vec![
                        Json::Str("10".into()),
                        Json::Str("=SUM((".into()),
                    ])]),
                ),
            ]),
        )
        .unwrap_err();
        assert!(
            err.contains("B1"),
            "error should name the offending cell: {err}"
        );
        // A1 was earlier in the same batch, but nothing was applied at all —
        // not even a no-op undo group landed on the stack.
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get_str("text"), Some("keep-me"));
        assert_eq!(a.undo.len(), undo_depth_before);
    }

    #[test]
    fn range_set_is_one_undo_group() {
        let mut a = app();
        dispatch(
            &mut a,
            "range.set",
            &Json::obj(vec![
                ("start", Json::Str("A1".into())),
                (
                    "rows",
                    Json::Arr(vec![
                        Json::Arr(vec![Json::Str("1".into()), Json::Str("2".into())]),
                        Json::Arr(vec![Json::Str("3".into()), Json::Str("4".into())]),
                    ]),
                ),
            ]),
        )
        .unwrap();
        a.undo(); // ONE undo call
        for r in ["A1", "B1", "A2", "B2"] {
            let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str(r.into()))])).unwrap();
            assert_eq!(g.get("value"), Some(&Json::Null), "{r} should be reverted");
        }
    }

    #[test]
    fn sheet_import_csv_creates_a_new_sheet_with_shape() {
        let mut a = app();
        let r = dispatch(
            &mut a,
            "sheet.import-csv",
            &Json::obj(vec![("text", Json::Str("name,amount\nAlice,30\n".into()))]),
        )
        .unwrap();
        assert_eq!(r.get_usize("rows"), Some(2)); // header + 1 data row
        assert_eq!(r.get_usize("cols"), Some(2));
        let idx = r.get_usize("sheet").unwrap();
        assert!(idx > 0); // never overwrites sheet 0
        let name = r.get_str("name").unwrap().to_string();
        assert_eq!(a.pkg.workbook.sheets[idx].name, name);
    }

    #[test]
    fn sheet_import_csv_twice_yields_two_distinct_sheets() {
        let mut a = app();
        let r1 = dispatch(
            &mut a,
            "sheet.import-csv",
            &Json::obj(vec![
                ("text", Json::Str("a\n1\n".into())),
                ("name", Json::Str("Data".into())),
            ]),
        )
        .unwrap();
        let r2 = dispatch(
            &mut a,
            "sheet.import-csv",
            &Json::obj(vec![
                ("text", Json::Str("a\n2\n".into())),
                ("name", Json::Str("Data".into())),
            ]),
        )
        .unwrap();
        assert_ne!(r1.get_usize("sheet"), r2.get_usize("sheet"));
        assert_ne!(r1.get_str("name"), r2.get_str("name"));
        assert_eq!(a.pkg.workbook.sheets.len(), 3);
    }

    #[test]
    fn sheet_import_csv_clears_the_undo_stack() {
        let mut a = app();
        set(&mut a, "A1", "1"); // an undoable edit exists
        dispatch(
            &mut a,
            "sheet.import-csv",
            &Json::obj(vec![("text", Json::Str("a\n1\n".into()))]),
        )
        .unwrap();
        a.undo();
        assert_eq!(a.status.as_deref(), Some("Nothing to undo"));
    }

    #[test]
    fn sheet_add_defaults_name_and_dedupes() {
        let mut a = app();
        let r1 = dispatch(&mut a, "sheet.add", &Json::Null).unwrap();
        let r2 = dispatch(&mut a, "sheet.add", &Json::Null).unwrap();
        assert_ne!(r1.get_str("name"), r2.get_str("name"));
        assert_eq!(a.pkg.workbook.sheets.len(), 3);
    }

    #[test]
    fn sheet_add_clears_the_undo_stack() {
        let mut a = app();
        set(&mut a, "A1", "1");
        dispatch(&mut a, "sheet.add", &Json::Null).unwrap();
        a.undo();
        assert_eq!(a.status.as_deref(), Some("Nothing to undo"));
    }

    #[test]
    fn sheet_remove_errors_on_the_last_sheet() {
        let mut a = app();
        let err = dispatch(
            &mut a,
            "sheet.remove",
            &Json::obj(vec![("sheet", Json::Num(0.0))]),
        )
        .unwrap_err();
        assert!(err.contains("last sheet"), "{err}");
    }

    #[test]
    fn sheet_remove_removes_the_named_sheet_and_clears_undo() {
        let mut a = app();
        dispatch(
            &mut a,
            "sheet.add",
            &Json::obj(vec![("name", Json::Str("Second".into()))]),
        )
        .unwrap();
        assert_eq!(a.pkg.workbook.sheets.len(), 2);
        set(&mut a, "A1", "1"); // an undoable edit exists
        let r = dispatch(
            &mut a,
            "sheet.remove",
            &Json::obj(vec![("sheet", Json::Str("Second".into()))]),
        )
        .unwrap();
        assert_eq!(r.get("removed").unwrap().as_bool(), Some(true));
        assert_eq!(a.pkg.workbook.sheets.len(), 1);
        a.undo();
        assert_eq!(a.status.as_deref(), Some("Nothing to undo"));
    }

    #[test]
    fn sheet_remove_resets_the_viewport_when_the_active_sheet_is_removed() {
        let mut a = app();
        dispatch(
            &mut a,
            "sheet.add",
            &Json::obj(vec![("name", Json::Str("Second".into()))]),
        )
        .unwrap();
        a.cur = (5, 3);
        a.top = 2;
        a.left = 1;
        a.anchor = Some((4, 4));
        // Remove the ACTIVE sheet (index 0) — the sheet the human was
        // looking at is gone, so the viewport must reset, same as the
        // TUI's own delete_current_sheet.
        dispatch(
            &mut a,
            "sheet.remove",
            &Json::obj(vec![("sheet", Json::Num(0.0))]),
        )
        .unwrap();
        assert_eq!(a.cur, (0, 0));
        assert_eq!(a.top, 0);
        assert_eq!(a.left, 0);
        assert_eq!(a.anchor, None);
    }

    #[test]
    fn sheet_remove_requires_an_explicit_sheet_arg() {
        let mut a = app();
        dispatch(
            &mut a,
            "sheet.add",
            &Json::obj(vec![("name", Json::Str("Second".into()))]),
        )
        .unwrap();
        let err = dispatch(&mut a, "sheet.remove", &Json::Null).unwrap_err();
        assert!(err.contains("sheet"));
    }

    #[test]
    fn sheet_remove_keeps_active_sheet_pointed_at_the_same_sheet() {
        let mut a = app();
        dispatch(
            &mut a,
            "sheet.add",
            &Json::obj(vec![("name", Json::Str("Second".into()))]),
        )
        .unwrap();
        dispatch(
            &mut a,
            "sheet.add",
            &Json::obj(vec![("name", Json::Str("Third".into()))]),
        )
        .unwrap();
        a.sheet = 2; // "Third" is active
        a.cur = (5, 3);
        a.top = 2;
        a.left = 1;
        a.anchor = Some((4, 4));
        dispatch(
            &mut a,
            "sheet.remove",
            &Json::obj(vec![("sheet", Json::Num(0.0))]),
        )
        .unwrap(); // remove Sheet1, before the active index
        assert_eq!(a.pkg.workbook.sheets[a.sheet].name, "Third");
        // The active sheet itself wasn't touched — its viewport/cursor/
        // selection must survive exactly as the human left them.
        assert_eq!(a.cur, (5, 3));
        assert_eq!(a.top, 2);
        assert_eq!(a.left, 1);
        assert_eq!(a.anchor, Some((4, 4)));
    }

    #[test]
    fn sheet_rename_updates_name_rewrites_refs_one_undo_group() {
        let mut a = app();
        dispatch(
            &mut a,
            "sheet.add",
            &Json::obj(vec![("name", Json::Str("Data".into()))]),
        )
        .unwrap();
        set(&mut a, "A1", "=Data!A1"); // Sheet1!A1 references the other sheet
        let r = dispatch(
            &mut a,
            "sheet.rename",
            &Json::obj(vec![
                ("sheet", Json::Str("Data".into())),
                ("name", Json::Str("Renamed".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_str("name"), Some("Renamed"));
        assert_eq!(a.pkg.workbook.sheets[1].name, "Renamed");
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get_str("formula"), Some("=Renamed!A1"));
        // One TUI-level undo reverts the whole rename (name + every formula).
        a.undo();
        assert_eq!(a.pkg.workbook.sheets[1].name, "Data");
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get_str("formula"), Some("=Data!A1"));
    }

    #[test]
    fn row_insert_shifts_a_formula_reference() {
        let mut a = app();
        set(&mut a, "A2", "5");
        set(&mut a, "B1", "=A2");
        let r = dispatch(
            &mut a,
            "row.insert",
            &Json::obj(vec![("at", Json::Num(0.0))]),
        )
        .unwrap();
        assert_eq!(r.get_usize("inserted"), Some(1));
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("B2".into()))])).unwrap();
        assert_eq!(g.get_str("formula"), Some("=A3"));
    }

    #[test]
    fn row_delete_removes_rows_with_structural_undo() {
        let mut a = app();
        set(&mut a, "A1", "1");
        set(&mut a, "A2", "2");
        let r = dispatch(
            &mut a,
            "row.delete",
            &Json::obj(vec![("at", Json::Num(0.0))]),
        )
        .unwrap();
        assert_eq!(r.get_usize("deleted"), Some(1));
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get("value").unwrap().as_f64(), Some(2.0)); // A2 shifted up
        a.undo();
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get("value").unwrap().as_f64(), Some(1.0));
    }

    #[test]
    fn col_insert_and_col_delete_report_counts() {
        let mut a = app();
        set(&mut a, "B1", "x");
        let r = dispatch(
            &mut a,
            "col.insert",
            &Json::obj(vec![("at", Json::Num(0.0)), ("count", Json::Num(2.0))]),
        )
        .unwrap();
        assert_eq!(r.get_usize("inserted"), Some(2));
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("D1".into()))])).unwrap();
        assert_eq!(g.get_str("text"), Some("x"));
        let r = dispatch(
            &mut a,
            "col.delete",
            &Json::obj(vec![("at", Json::Num(0.0)), ("count", Json::Num(2.0))]),
        )
        .unwrap();
        assert_eq!(r.get_usize("deleted"), Some(2));
    }

    #[test]
    fn wb_replace_all_touches_every_sheet_in_one_undo_group() {
        let mut a = app();
        set(&mut a, "A1", "foo bar");
        dispatch(
            &mut a,
            "sheet.add",
            &Json::obj(vec![("name", Json::Str("Second".into()))]),
        )
        .unwrap();
        a.sheet = 1;
        set(&mut a, "A1", "foo baz");
        let r = dispatch(
            &mut a,
            "wb.replace-all",
            &Json::obj(vec![
                ("query", Json::Str("foo".into())),
                ("text", Json::Str("QUX".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("replaced"), Some(2));
        let g0 = cell_get(
            &a,
            &Json::obj(vec![
                ("ref", Json::Str("A1".into())),
                ("sheet", Json::Num(0.0)),
            ]),
        )
        .unwrap();
        assert_eq!(g0.get_str("text"), Some("QUX bar"));
        let g1 = cell_get(
            &a,
            &Json::obj(vec![
                ("ref", Json::Str("A1".into())),
                ("sheet", Json::Num(1.0)),
            ]),
        )
        .unwrap();
        assert_eq!(g1.get_str("text"), Some("QUX baz"));
        // One undo restores BOTH sheets — proof it's a single undo group.
        a.undo();
        let g0 = cell_get(
            &a,
            &Json::obj(vec![
                ("ref", Json::Str("A1".into())),
                ("sheet", Json::Num(0.0)),
            ]),
        )
        .unwrap();
        assert_eq!(g0.get_str("text"), Some("foo bar"));
        let g1 = cell_get(
            &a,
            &Json::obj(vec![
                ("ref", Json::Str("A1".into())),
                ("sheet", Json::Num(1.0)),
            ]),
        )
        .unwrap();
        assert_eq!(g1.get_str("text"), Some("foo baz"));
    }

    #[test]
    fn wb_replace_all_rejects_empty_query() {
        let mut a = app();
        let err = dispatch(
            &mut a,
            "wb.replace-all",
            &Json::obj(vec![
                ("query", Json::Str("".into())),
                ("text", Json::Str("x".into())),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("query"));
    }

    // -----------------------------------------------------------------
    // Wave-2 cell.format / col.width
    // -----------------------------------------------------------------

    #[test]
    fn cell_format_bold_and_fill_over_a_2x2_range_reports_formatted_count() {
        let mut a = app();
        let r = dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1:B2".into())),
                (
                    "patch",
                    Json::obj(vec![
                        ("bold", Json::Bool(true)),
                        ("fillColor", Json::Str("#FFFF00".into())),
                    ]),
                ),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("formatted"), Some(4));
    }

    #[test]
    fn cell_format_rejects_a_range_over_the_cap_and_touches_nothing() {
        let mut a = app();
        set(&mut a, "A1", "keep-me");
        let undo_depth_before = a.undo.len();
        // A1:A1048576 — the whole column — vastly exceeds the cap; must be
        // rejected BEFORE materializing a Cell per coordinate.
        let err = dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1:A1048576".into())),
                ("patch", Json::obj(vec![("bold", Json::Bool(true))])),
            ]),
        )
        .unwrap_err();
        assert_eq!(
            err,
            format!(
                "cell.format: range too large (limit {} cells)",
                gridcore::format::CELL_FORMAT_CAP
            )
        );
        // Nothing applied: the pre-existing cell is untouched and no undo
        // group landed on the stack.
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get_str("text"), Some("keep-me"));
        assert!(g.get("format").is_none());
        assert_eq!(a.undo.len(), undo_depth_before);
    }

    #[test]
    fn cell_format_at_the_cap_still_works() {
        let mut a = app();
        // A 50x100 rectangle lands exactly at the cap: allowed, and every
        // cell in it gets formatted.
        let cols: u32 = 50;
        let rows: u64 = gridcore::format::CELL_FORMAT_CAP / u64::from(cols);
        assert_eq!(rows * u64::from(cols), gridcore::format::CELL_FORMAT_CAP);
        let range = format!("A1:{}{rows}", gridcore::sheet::col_name(cols - 1));
        let r = dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str(range)),
                ("patch", Json::obj(vec![("bold", Json::Bool(true))])),
            ]),
        )
        .unwrap();
        assert_eq!(
            r.get_usize("formatted"),
            Some(gridcore::format::CELL_FORMAT_CAP as usize)
        );
    }

    #[test]
    fn cell_get_format_echoes_the_patch_on_every_formatted_cell() {
        let mut a = app();
        dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1:B2".into())),
                (
                    "patch",
                    Json::obj(vec![
                        ("bold", Json::Bool(true)),
                        ("fillColor", Json::Str("#FFFF00".into())),
                    ]),
                ),
            ]),
        )
        .unwrap();
        for r in ["A1", "A2", "B1", "B2"] {
            let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str(r.into()))])).unwrap();
            let fmt = g
                .get("format")
                .unwrap_or_else(|| panic!("{r} should carry a format"));
            assert_eq!(fmt.get("bold").unwrap().as_bool(), Some(true));
            assert_eq!(fmt.get_str("fillColor"), Some("#FFFF00"));
        }
    }

    #[test]
    fn cell_format_preserves_existing_cell_value() {
        let mut a = app();
        set(&mut a, "A1", "42");
        dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1".into())),
                ("patch", Json::obj(vec![("bold", Json::Bool(true))])),
            ]),
        )
        .unwrap();
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert_eq!(g.get("value").unwrap().as_f64(), Some(42.0));
        assert_eq!(
            g.get("format").unwrap().get("bold").unwrap().as_bool(),
            Some(true)
        );
    }

    #[test]
    fn cell_format_is_one_undo_group_and_undo_clears_the_format_key() {
        let mut a = app();
        dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1:B2".into())),
                (
                    "patch",
                    Json::obj(vec![
                        ("bold", Json::Bool(true)),
                        ("fillColor", Json::Str("#FFFF00".into())),
                    ]),
                ),
            ]),
        )
        .unwrap();
        a.undo(); // ONE undo call
        for r in ["A1", "A2", "B1", "B2"] {
            let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str(r.into()))])).unwrap();
            assert!(
                g.get("format").is_none(),
                "{r} should have no format key after undo, got {g:?}"
            );
        }
    }

    #[test]
    fn cell_get_reports_no_format_key_for_an_unstyled_cell() {
        let mut a = app();
        set(&mut a, "A1", "plain");
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert!(g.get("format").is_none());
    }

    /// Unlike `app()` (built from an in-memory `new_xlsx()` directly, never
    /// round-tripped through `load_xlsx`), this proves the "no format key
    /// for an unstyled cell" contract through the ACTUAL load path every
    /// real `.xlsx` goes through — the path that exposed
    /// `gridcore::format::xf_format_fields`'s "General" numFmt leak (see
    /// gridcore's `xf_format_fields` doc comment and its
    /// `xf_format_fields_is_empty_for_a_loaded_workbooks_untouched_default_style`
    /// test). `App::new(new_xlsx(), …)`'s in-memory `Xf::default()` has
    /// `code: None`, so it never surfaced the bug in the first place; a
    /// genuinely loaded workbook's style index 0 has `code:
    /// Some("General")` (synthesized by `crate::xlsx`'s `<cellXfs>` parser
    /// for round-trip fidelity) and is exactly what regressed before the
    /// fix.
    #[test]
    fn cell_get_reports_no_format_key_for_an_unstyled_cell_on_a_loaded_workbook() {
        let bytes = gridcore::xlsx::save_xlsx(&gridcore::xlsx::new_xlsx());
        let pkg = gridcore::xlsx::load_xlsx(&bytes).expect("round trip");
        let mut a = App::new(pkg, "loaded-test.xlsx");
        a.os_clip = None;
        set(&mut a, "A1", "plain");
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert!(
            g.get("format").is_none(),
            "an untouched cell on a REAL loaded workbook must have no format key: {g:?}"
        );
    }

    #[test]
    fn cell_format_on_a_loaded_workbook_does_not_leak_a_general_numfmt() {
        let bytes = gridcore::xlsx::save_xlsx(&gridcore::xlsx::new_xlsx());
        let pkg = gridcore::xlsx::load_xlsx(&bytes).expect("round trip");
        let mut a = App::new(pkg, "loaded-test.xlsx");
        a.os_clip = None;
        let r = dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1".into())),
                ("patch", Json::obj(vec![("bold", Json::Bool(true))])),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("formatted"), Some(1));
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        let fmt = g.get("format").expect("format key present");
        assert_eq!(
            fmt.get("bold").and_then(Json::as_bool),
            Some(true),
            "{fmt:?}"
        );
        assert!(
            fmt.get("numFmt").is_none(),
            "a patch that never touches numFmt must not inherit the loaded \
             default style's synthesized 'General' code: {fmt:?}"
        );
    }

    #[test]
    fn cell_format_unknown_key_names_it_and_applies_nothing() {
        let mut a = app();
        let err = dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1".into())),
                ("patch", Json::obj(vec![("wrap", Json::Bool(true))])),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("wrap"), "{err}");
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert!(g.get("format").is_none());
    }

    #[test]
    fn cell_format_empty_patch_errors() {
        let mut a = app();
        let err = dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1".into())),
                ("patch", Json::obj(vec![])),
            ]),
        )
        .unwrap_err();
        assert_eq!(err, "patch needs at least one key");
    }

    #[test]
    fn cell_format_bad_num_fmt_errors_and_applies_nothing() {
        let mut a = app();
        let err = dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1".into())),
                (
                    "patch",
                    Json::obj(vec![("numFmt", Json::Str("[[[not a format".into()))]),
                ),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("numFmt"), "{err}");
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert!(g.get("format").is_none());
    }

    #[test]
    fn cell_format_bad_color_errors() {
        let mut a = app();
        let err = dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1".into())),
                (
                    "patch",
                    Json::obj(vec![("fontColor", Json::Str("red".into()))]),
                ),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("color"), "{err}");
    }

    #[test]
    fn cell_format_align_and_numfmt_round_trip_through_cell_get() {
        let mut a = app();
        dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1".into())),
                (
                    "patch",
                    Json::obj(vec![
                        ("align", Json::Str("center".into())),
                        ("numFmt", Json::Str("0.00%".into())),
                        ("italic", Json::Bool(true)),
                    ]),
                ),
            ]),
        )
        .unwrap();
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        let fmt = g.get("format").unwrap();
        assert_eq!(fmt.get_str("align"), Some("center"));
        assert_eq!(fmt.get_str("numFmt"), Some("0.00%"));
        assert_eq!(fmt.get("italic").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn cell_format_rejects_a_non_object_patch() {
        let mut a = app();
        let err = dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1".into())),
                ("patch", Json::Str("bold".into())),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("patch"), "{err}");
    }

    /// The format read-back is deliberately scoped to `cell.get` alone (per
    /// spec: read-modify-write is the only use case). A styled cell's
    /// `format` object must NOT leak into `sheet.read`, `find`, or
    /// `cell.set`'s reply — even though all three share the underlying
    /// `cell_json` builder with `cell.get`. This pins the scope so Task 4's
    /// gridwasm mirror matches exactly.
    #[test]
    fn format_read_back_is_scoped_to_cell_get_only() {
        let mut a = app();
        dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("A1".into())),
                ("patch", Json::obj(vec![("bold", Json::Bool(true))])),
            ]),
        )
        .unwrap();

        // cell.set's own reply, on the now-styled cell: no format key.
        let set_reply = dispatch(
            &mut a,
            "cell.set",
            &Json::obj(vec![
                ("ref", Json::Str("A1".into())),
                ("text", Json::Str("hello".into())),
            ]),
        )
        .unwrap();
        assert!(set_reply.get("format").is_none(), "{set_reply:?}");

        // cell.get: format IS present (the one carrier of this key).
        let g = cell_get(&a, &Json::obj(vec![("ref", Json::Str("A1".into()))])).unwrap();
        assert!(g.get("format").is_some());

        // sheet.read: no format key on the same styled cell's entry.
        let sr = sheet_read(&a, &Json::Null).unwrap();
        let cells = sr.get("cells").unwrap().as_array().unwrap();
        let a1 = cells
            .iter()
            .find(|c| c.get_str("ref") == Some("A1"))
            .expect("A1 present in sheet.read");
        assert!(a1.get("format").is_none(), "{a1:?}");

        // find: no format key on the matched cell either.
        let found = find(&a, &Json::obj(vec![("query", Json::Str("hello".into()))])).unwrap();
        let matches = found.get("matches").unwrap().as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].get("format").is_none(), "{:?}", matches[0]);
    }

    #[test]
    fn col_width_sets_and_is_readable_from_the_sheet() {
        let mut a = app();
        let r = dispatch(
            &mut a,
            "col.width",
            &Json::obj(vec![
                ("col", Json::Str("C".into())),
                ("width", Json::Num(20.0)),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("col"), Some(2));
        assert_eq!(r.get("width").unwrap().as_f64(), Some(20.0));
        assert_eq!(a.pkg.workbook.sheets[0].col_width(2), 20.0);
    }

    #[test]
    fn col_width_accepts_a_0_based_index_too() {
        let mut a = app();
        dispatch(
            &mut a,
            "col.width",
            &Json::obj(vec![("col", Json::Num(4.0)), ("width", Json::Num(15.0))]),
        )
        .unwrap();
        assert_eq!(a.pkg.workbook.sheets[0].col_width(4), 15.0);
    }

    #[test]
    fn col_width_rejects_a_huge_numeric_col() {
        let mut a = app();
        let err = dispatch(
            &mut a,
            "col.width",
            &Json::obj(vec![
                ("col", Json::Num(99_999_999.0)),
                ("width", Json::Num(20.0)),
            ]),
        )
        .unwrap_err();
        assert_eq!(err, "bad column '99999999'");
    }

    #[test]
    fn col_width_accepts_a_digit_string_col_and_bounds_it() {
        let mut a = app();
        // A schema-conforming string index ("5"), not just a JSON number.
        dispatch(
            &mut a,
            "col.width",
            &Json::obj(vec![
                ("col", Json::Str("5".into())),
                ("width", Json::Num(12.0)),
            ]),
        )
        .unwrap();
        assert_eq!(a.pkg.workbook.sheets[0].col_width(5), 12.0);

        // But the same bound applies as the numeric arm.
        let err = dispatch(
            &mut a,
            "col.width",
            &Json::obj(vec![
                ("col", Json::Str("99999999".into())),
                ("width", Json::Num(12.0)),
            ]),
        )
        .unwrap_err();
        assert_eq!(err, "bad column '99999999'");
    }

    #[test]
    fn col_width_letter_arm_is_unchanged() {
        let mut a = app();
        // A valid letter still resolves exactly as before.
        let r = dispatch(
            &mut a,
            "col.width",
            &Json::obj(vec![
                ("col", Json::Str("Z".into())),
                ("width", Json::Num(9.0)),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("col"), Some(25));
        // An out-of-range letter is still rejected the same way it always was.
        let err = dispatch(
            &mut a,
            "col.width",
            &Json::obj(vec![
                ("col", Json::Str("ZZZZ".into())),
                ("width", Json::Num(9.0)),
            ]),
        )
        .unwrap_err();
        assert_eq!(err, "bad column 'ZZZZ'");
    }

    #[test]
    fn col_width_rejects_non_positive_width() {
        let mut a = app();
        let err = dispatch(
            &mut a,
            "col.width",
            &Json::obj(vec![
                ("col", Json::Str("A".into())),
                ("width", Json::Num(0.0)),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("positive"), "{err}");
        let err = dispatch(
            &mut a,
            "col.width",
            &Json::obj(vec![
                ("col", Json::Str("A".into())),
                ("width", Json::Num(-5.0)),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("positive"), "{err}");
    }

    #[test]
    fn col_width_is_not_on_the_undo_stack() {
        // Empirical fact this verb mirrors: the TUI's own F7/F8 width-adjust
        // keys mutate `Sheet::set_col_width` directly, with no `self.undo`
        // entry. `col.width` matches that — a human's Ctrl+Z in the TUI must
        // not touch a column width an agent (or the keyboard) just set.
        let mut a = app();
        let undo_depth_before = a.undo.len();
        dispatch(
            &mut a,
            "col.width",
            &Json::obj(vec![
                ("col", Json::Str("A".into())),
                ("width", Json::Num(20.0)),
            ]),
        )
        .unwrap();
        assert_eq!(a.undo.len(), undo_depth_before);
        assert!(a.modified);
    }

    // -----------------------------------------------------------------
    // Wave-3 pivot.create
    // -----------------------------------------------------------------

    fn pivot_create_args(name: Option<&str>) -> Json {
        let mut fields = vec![
            ("range", Json::Str("A1:B4".into())),
            ("rows", Json::Arr(vec![Json::Str("name".into())])),
            (
                "values",
                Json::Arr(vec![Json::obj(vec![
                    ("col", Json::Str("amount".into())),
                    ("agg", Json::Str("sum".into())),
                ])]),
            ),
        ];
        if let Some(n) = name {
            fields.push(("name", Json::Str(n.into())));
        }
        Json::obj(fields)
    }

    #[test]
    fn pivot_create_returns_sheet_and_name_lists_and_computes() {
        let mut a = app();
        pivot_fixture(&mut a);
        let r = dispatch(&mut a, "pivot.create", &pivot_create_args(None)).unwrap();
        assert_eq!(r.get_str("name"), Some("Pivot1"));
        let dest = r.get_usize("sheet").unwrap();
        assert_ne!(dest, 0, "the pivot must land on a NEW sheet");
        assert_eq!(a.pkg.workbook.sheets[dest].name, "Pivot1");

        // pivot.list includes it.
        let lst = dispatch(&mut a, "pivot.list", &Json::Null).unwrap();
        let pivots = lst.get("pivots").unwrap().as_array().unwrap();
        assert_eq!(pivots.len(), 1);
        assert_eq!(pivots[0].get_usize("sheet"), Some(dest));
        let rows = pivots[0].get("rows").unwrap().as_array().unwrap();
        assert_eq!(rows[0].as_str(), Some("name"));
        let values = pivots[0].get("values").unwrap().as_array().unwrap();
        assert_eq!(values[0].as_str(), Some("Sum of amount"));

        // The output sheet already holds computed values (row 2 = header,
        // row 3 = first row group — "Alice" sorts first, total 10+20=30).
        let g = cell_get(
            &a,
            &Json::obj(vec![
                ("ref", Json::Str("B4".into())),
                ("sheet", Json::Num(dest as f64)),
            ]),
        )
        .unwrap();
        assert_eq!(g.get("value").unwrap().as_f64(), Some(30.0)); // Alice: 10+20
    }

    #[test]
    fn pivot_create_default_names_are_unique_pivotn() {
        let mut a = app();
        pivot_fixture(&mut a);
        let r1 = dispatch(&mut a, "pivot.create", &pivot_create_args(None)).unwrap();
        assert_eq!(r1.get_str("name"), Some("Pivot1"));
        let r2 = dispatch(&mut a, "pivot.create", &pivot_create_args(None)).unwrap();
        assert_eq!(r2.get_str("name"), Some("Pivot2"));
    }

    #[test]
    fn pivot_create_explicit_name_is_used_and_duplicate_errors() {
        let mut a = app();
        pivot_fixture(&mut a);
        let r = dispatch(&mut a, "pivot.create", &pivot_create_args(Some("ByName"))).unwrap();
        assert_eq!(r.get_str("name"), Some("ByName"));
        let err = dispatch(&mut a, "pivot.create", &pivot_create_args(Some("ByName"))).unwrap_err();
        assert!(err.contains("ByName"), "{err}");
        // Colliding with a plain (non-pivot) sheet name errors the same way.
        let err = dispatch(&mut a, "pivot.create", &pivot_create_args(Some("Sheet1"))).unwrap_err();
        assert!(err.contains("Sheet1"), "{err}");
    }

    #[test]
    fn pivot_create_unknown_header_names_the_column_like_sheet_pivot() {
        let mut a = app();
        pivot_fixture(&mut a);
        let err = dispatch(
            &mut a,
            "pivot.create",
            &Json::obj(vec![
                ("range", Json::Str("A1:B4".into())),
                ("rows", Json::Arr(vec![Json::Str("nope".into())])),
                (
                    "values",
                    Json::Arr(vec![Json::obj(vec![
                        ("col", Json::Str("amount".into())),
                        ("agg", Json::Str("sum".into())),
                    ])]),
                ),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("nope"), "error should name the column: {err}");
        assert!(err.starts_with("pivot.create:"), "{err}");
    }

    #[test]
    fn pivot_create_needs_at_least_one_value_field() {
        let mut a = app();
        pivot_fixture(&mut a);
        let err = dispatch(
            &mut a,
            "pivot.create",
            &Json::obj(vec![
                ("range", Json::Str("A1:B4".into())),
                ("rows", Json::Arr(vec![Json::Str("name".into())])),
                ("values", Json::Arr(vec![])),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("value field"), "{err}");
    }

    #[test]
    fn pivot_create_clears_the_undo_stack() {
        // Empirical fact (Wave-3 Task 3): mirrors sheet.add/sheet.import-csv
        // — the new sheet + pivot-part registration isn't a cell-level edit
        // the undo stack can invert, so like those verbs it clears history
        // rather than push an entry.
        let mut a = app();
        pivot_fixture(&mut a);
        set(&mut a, "D1", "1"); // an undoable edit exists beforehand
        dispatch(&mut a, "pivot.create", &pivot_create_args(None)).unwrap();
        a.undo();
        assert_eq!(a.status.as_deref(), Some("Nothing to undo"));
    }

    #[test]
    fn pivot_create_source_edit_and_recalc_refreshes_the_output() {
        let mut a = app();
        pivot_fixture(&mut a);
        let r = dispatch(&mut a, "pivot.create", &pivot_create_args(None)).unwrap();
        let dest = r.get_usize("sheet").unwrap();
        // Bob's amount 20 -> 200 (Bob is the second row group, at B5 — see
        // the previous test's layout note: row2=header, row3=Alice, row4=Bob).
        set(&mut a, "B3", "200");
        dispatch(&mut a, "wb.recalc", &Json::Null).unwrap();
        let g = cell_get(
            &a,
            &Json::obj(vec![
                ("ref", Json::Str("B5".into())),
                ("sheet", Json::Num(dest as f64)),
            ]),
        )
        .unwrap();
        assert_eq!(g.get("value").unwrap().as_f64(), Some(200.0));
    }

    #[test]
    fn pivot_create_survives_save_load_and_is_still_refreshable() {
        let tmp = std::env::temp_dir().join("xlsxy_pivot_create_round_trip.xlsx");
        let _ = std::fs::remove_file(&tmp);
        let mut a = App::new(new_xlsx(), tmp.to_str().unwrap());
        a.os_clip = None;
        pivot_fixture(&mut a);
        let r = dispatch(&mut a, "pivot.create", &pivot_create_args(None)).unwrap();
        let dest = r.get_usize("sheet").unwrap();
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();

        // A different session opens the saved file.
        let mut b = App::new(new_xlsx(), "other.xlsx");
        b.os_clip = None;
        dispatch(
            &mut b,
            "wb.open",
            &Json::obj(vec![("path", Json::Str(tmp.to_str().unwrap().into()))]),
        )
        .unwrap();
        assert_eq!(b.pkg.workbook.pivots.len(), 1, "pivot lost on reload");
        assert_eq!(b.pkg.workbook.sheets[dest].name, "Pivot1");
        let lst = dispatch(&mut b, "pivot.list", &Json::Null).unwrap();
        assert_eq!(lst.get("pivots").unwrap().as_array().unwrap().len(), 1);
        // Still refreshable: editing the source and recalculating updates
        // the reloaded output sheet (Bob's row is B5 — see the layout note
        // in pivot_create_returns_sheet_and_name_lists_and_computes).
        set(&mut b, "B3", "200");
        dispatch(&mut b, "wb.recalc", &Json::Null).unwrap();
        let g = cell_get(
            &b,
            &Json::obj(vec![
                ("ref", Json::Str("B5".into())),
                ("sheet", Json::Num(dest as f64)),
            ]),
        )
        .unwrap();
        assert_eq!(g.get("value").unwrap().as_f64(), Some(200.0));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn pivot_create_removing_its_sheet_also_drops_the_pivot_registration() {
        // The "both or neither" inverse contract: sheet.remove on the
        // pivot's own sheet must not leave a dangling pivot.list entry.
        let mut a = app();
        pivot_fixture(&mut a);
        let r = dispatch(&mut a, "pivot.create", &pivot_create_args(None)).unwrap();
        let dest = r.get_usize("sheet").unwrap();
        dispatch(
            &mut a,
            "sheet.remove",
            &Json::obj(vec![("sheet", Json::Num(dest as f64))]),
        )
        .unwrap();
        let lst = dispatch(&mut a, "pivot.list", &Json::Null).unwrap();
        assert_eq!(lst.get("pivots").unwrap().as_array().unwrap().len(), 0);
    }
}
