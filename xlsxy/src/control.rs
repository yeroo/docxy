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
//! | `wb.recalc` | — | `{recalculated:true}` |
//! | `wb.save` | — | `{path, …}` |
//! | `wb.reload` | — | `{path, …}` |
//! | `wb.open` | `{path}` | `{path, …}` |

use crate::{App, parse_input};
use ctlcore::json::Json;
use gridcore::engine::Engine;
use gridcore::sheet::{Cell, CellValue, cell_name, fmt_general, parse_cell_name, parse_range_name};

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
        if matches!(verb, "cell.set" | "range.clear") {
            ctlcore::signal_activity();
        }
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

/// One cell as JSON: `ref`, coordinates, the typed `value`, the formula source
/// (with `=`), and `text` (the general-format display string).
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
        Some(cell) => Ok(cell_json(r, c, cell)),
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
}
