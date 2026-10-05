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
//! | `wb.path` | — | `{path, modified, sheets, active, active_name, circular}` |
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
//! | `sheet.import-csv` | `{text,name?}` | `{sheet,name,rows,cols}` — always a new sheet; fields convert as a `.csv` open does (`sep=`, typed-entry rules, File › Options › Data) |
//! | `sheet.import-text` | `{text\|path,options?,name?}` | `{sheet,name,rows,cols}` — the Text Import Wizard's options (see `text_options`) into a new sheet |
//! | `app.options` | `{convert_leading_zeros?,convert_long_numbers?,convert_e_notation?,convert_dates?,edit_fixed_decimal?,edit_fixed_decimal_places?,edit_move_after_enter?,edit_move_direction?,edit_in_cell?,edit_autocomplete?}` | all of them — File › Options › Data › Automatic Data Conversion and Advanced › Editing (places -300..=300, direction `down`/`right`/`up`/`left`); every given key is checked before any is set |
//! | `range.text-to-columns` | `{range,options?,dest?,replace?,sheet?}` | `{rows}` — one column; refuses with "Do you want to replace the contents of the destination cells?" unless `replace:true`; one undo step |
//! | `wb.consolidate` | `{refs,dest?,fn?,top?,left?,links?}` | `{sheet,range}` — Data › Consolidate: `refs` like `East!A1:C4` (a bare range is on the destination's sheet); `dest` a cell, `Sheet!B2` or `B2` (default: the cursor on the active sheet); `fn` a dialog name or file token (`Sum` default, `Count Numbers`/`countNums`, …); `top`/`left` match by labels; `links` writes linked formulas in an outline (refused for a source on the destination sheet). A refusal changes nothing. One undo step |
//! | `wb.replace-all` | `{query,text}` | `{replaced}` — every sheet, one undo group |
//! | `sheet.add` | `{name?}` | `{sheet,name}` |
//! | `sheet.remove` | `{sheet}` | `{removed:true}` (last-sheet error) |
//! | `sheet.rename` | `{sheet,name}` | `{name}` |
//! | `table.list` | — | `{tables:[{name,sheet,sheet_name,ref,columns,header_rows,totals_rows}]}` |
//! | `table.rename` | `{name,new}` | the table, as `table.list` shows it — Excel's name rules; every formula naming it follows. One undo step |
//! | `table.resize` | `{name,ref}` | the table — the header row stays, the new range overlaps the old and keeps a data row; a table with a Total Row keeps its bottom row; new columns' unique names are written into their header cells. One undo step |
//! | `table.convert` | `{name}` | `{converted}` — structured references become cell references; refused while a PivotTable, a SUMX-style formula or the data model uses the table. One undo step |
//! | `row.insert` / `row.delete` | `{at,count?,sheet?}` | `{inserted\|deleted}` |
//! | `col.insert` / `col.delete` | `{at,count?,sheet?}` | `{inserted\|deleted}` |
//! | `cell.format` | `{range,patch,sheet?}` | `{formatted}` — one undo group; `patch` keys: `numFmt`/`bold`/`italic`/`fontColor`/`fillColor`/`align` (≥1 required) |
//! | `col.width` | `{col,width,sheet?}` | `{col,width}` — NOT on the undo stack (mirrors the TUI's F7/F8, which mutate directly) |
//! | `page.setup` | `{sheet?\|sheets?, …fields}` | the sheet's page layout. Settable fields: `margins:{left,right,top,bottom,header,footer}` (inches), `paperSize`, `orientation`, `scale`, `fitToPage`, `fitToWidth`, `fitToHeight` (0 = Automatic), `firstPageNumber` (`null` = Auto), `pageOrder`, `blackAndWhite`, `draft`, `cellComments`, `errors`, `gridLines`, `headings`, `horizontalCentered`, `verticalCentered`, `differentOddEven`, `differentFirst`, `scaleWithDoc`, `alignWithMargins`. Reply-only: `headers:{oddHeader…firstFooter}` (stored codes; set with `page.header`), `printArea` (`print-area.*`), `printTitles:{rows,cols}` (`print-titles.set`), `rowBreaks`, `colBreaks` (`page-break.*`). Any settable field given sets it (+ `changed`): a fit count turns `fitToPage` on and `scale` turns it off unless `fitToPage` is given; scale outside 10–400 is refused. With `sheets`, the first sheet's page setup is then copied to the others (grouped sheets: not print areas, titles or header pictures). One undo step |
//! | `page.header` | `{sheet?, kind?:odd\|even\|first, part?:header\|footer, left?, center?, right?}` | `{stored, left, center, right, changed}` — sections in the editor's form (`&[Page]`, `&[Pages]`, `&[Date]`, `&[Time]`, `&[Path]`, `&[File]`, `&[Tab]`), stored as Excel's codes; with none of left/center/right it only reads; a section over 255 characters is refused; `&[Picture]` only where the section already has a picture; `&L`, `&C` or `&R` inside a section is refused (a literal ampersand is `&&`). One undo step |
//! | `print-area.set` / `print-area.add` | `{range, sheet?}` | `{printArea, changed}` — `range` is `A1:C10`, `A1:C10,E1:F5`, `A:C` or `1:5`; add appends to the sheet's print area. One undo step |
//! | `print-area.clear` | `{sheet?}` | `{printArea:null, changed}` |
//! | `print-titles.set` | `{rows?, cols?, sheet?}` | `{printTitles:{rows,cols}, changed}` — `"1:2"` / `"A:A"`; an absent key keeps that part, `null` or `""` clears it; titles that would fill a page by themselves, at the print scale, don't repeat |
//! | `page-break.insert` / `page-break.remove` | `{cell, sheet?}` | `{rowBreaks, colBreaks, changed}` — manual break ids (a row break before 0-based row id); insert adds a break above the cell (not in row 1) and left of it (not in column A); remove takes the manual breaks bordering it |
//! | `page-break.reset` | `{sheet?}` | `{rowBreaks, colBreaks, changed}` — every manual break goes |
//! | `print.pages` | `{what?:active\|workbook\|selection, sheet?\|sheets?, range?, ignorePrintAreas?, from?, to?}` | `{total, pages:[{sheet, name, range, number, titleRows, titleCols, scale}]}` — the pages printing lays out (hidden sheets print only when named; titles that would fill a page by themselves, at the print scale, don't repeat); `selection` needs `range` (only its printed cells print); a job over 100,000 pages errors with "This would print more than 100000 pages; …" |
//! | `wb.export-pdf` | `{path, …print.pages args}` | `{path, pages}` — refuses to overwrite; nothing to print errors with "We didn't find anything to print." and writes no file; so does a job over 100,000 pages (the `print.pages` error) |
//! | `filter.set` | `{range?, col, criteria, sheet?}` | `{shown, total, status, changed}` — AutoFilter on over `range` if needed, then column `col` (header text or letter) set to `criteria` (`values`, `custom`, `top`, `dynamic`, `cellColor`, `fontColor`, `icon`, `search`, or `null` to clear) and the filter applied. One undo step when it changes something |
//! | `filter.reapply` / `filter.clear` / `filter.off` | `{col?, sheet?}` | `{shown, total, status, changed}` (`filter.off`: `{off, range, changed}`) — one undo step each when it changes something |
//! | `filter.menu` | `{col, search?, sheet?}` | `{col, header, submenu, items:[{label,depth,checked}], truncated, total, filtered, colors}` |
//! | `filter.by-cell` | `{ref, by?, sheet?}` | `{shown, total, status, changed}` — Filter by Selected Cell's value/colour/font colour/icon |
//! | `filter.advanced` | `{list, criteria?, copyTo?, unique?, sheet?}` | `{shown, total, status, changed}` — copying to another sheet is refused |
//! | `sheet.rows` | `{range, sheet?}` | `{rows:[{row, hidden, hiddenBy}]}` |
//! | `range.sort` | `{range, keys, header?, caseSensitive?, orientation?, expand?, sheet?}` | `{sorted, range, count, changed}`, or `{sorted:false, warning, expanded}` for a selection inside a wider list |
//! | `wb.clock` | `{date}` | `{date}` — fixes today (`null`: the local clock) |
//! | `wb.recalc` | — | `{recalculated:true}` |
//! | `wb.properties` | — | `{title, tags, categories, subject, comments, company, manager, hyperlinkBase, author, lastModifiedBy, created, modified, custom:[{name,type,value}]}` — File › Info; an absent property is `null`; `type` is `text`/`number`/`bool`/`date`/`other` |
//! | `wb.set-properties` | `{title?, tags?, categories?, subject?, comments?, company?, manager?, hyperlinkBase?, custom?:{name: value\|null}}` | `wb.properties` + `{changed}` — `null`/`""` removes; a custom value is a string (text), number, bool or `{"date":"YYYY-MM-DD[THH:MM:SSZ]"}`; marks the workbook modified when something changed; NOT on the undo stack (Excel's Info edits aren't either) |
//! | `wb.save` | — | `{path, …}`; a failed write errors with `save failed: …` (the status-bar text) and the workbook stays modified |
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

use crate::{App, comment_author, iso_now, now_serial};
use ctlcore::json::Json;
use gridcore::docprops::{CustomProperty, CustomValue};
use gridcore::engine::{Engine, PART_OF_ARRAY, cell_to_value, eval_formula_at};
use gridcore::format::{FormatPatch, FormatValue, apply_patch_to_xf, xf_format_fields};
use gridcore::formula::Value;
use gridcore::frame::{Agg, Frame, pivot, pivot_spec_from_names, pivot_table_strings, range_stats};
use gridcore::print::area;
use gridcore::print::hf;
use gridcore::print::paginate::{Job, What};
use gridcore::print::setup::{
    CellComments, HfSlot, Orientation, PageOrder, PageSetup, PrintErrors,
};
use gridcore::sheet::{
    Cell, CellValue, DrawingKind, Styles, cell_name, col_name, fmt_general, parse_cell_name,
    parse_col, parse_range_name, sheet_to_csv,
};

/// The most cells one `sheet.read` returns (non-empty cells in the window);
/// larger reads set `truncated: true` so a client narrows the range.
const READ_CAP: usize = 5000;
/// The most matches one `find` returns.
const FIND_CAP: usize = 200;

mod datacmds;

/// Whether `verb` is an agent edit of the workbook: its cells, sheets,
/// tables or layout.
fn edits(verb: &str) -> bool {
    matches!(
        verb,
        "cell.set"
            | "range.clear"
            | "comment.add"
            | "range.set"
            | "sheet.import-csv"
            | "sheet.import-text"
            | "range.text-to-columns"
            | "wb.replace-all"
            | "wb.consolidate"
            | "sheet.add"
            | "sheet.remove"
            | "sheet.rename"
            | "table.rename"
            | "table.resize"
            | "table.convert"
            | "pivot.create"
            | "row.insert"
            | "row.delete"
            | "col.insert"
            | "col.delete"
            | "cell.format"
            | "col.width"
    )
}

/// Whether `verb` changes the workbook: an edit ([`edits`]), a filter or a
/// sort (which hide or move rows, and signal activity themselves, only when
/// they changed something), or another workbook opened or read back in its
/// place. What a dialog over it shows may then be gone.
pub fn mutates(verb: &str) -> bool {
    edits(verb)
        || matches!(
            verb,
            "filter.set"
                | "filter.reapply"
                | "filter.clear"
                | "filter.off"
                | "filter.by-cell"
                | "filter.advanced"
                | "range.sort"
                | "wb.open"
                | "wb.reload"
        )
}

/// Whether `verb`, when it [`mutates`] the workbook, leaves every cell and
/// sheet where it was: it writes, formats or annotates cells in place, or
/// appends a sheet. A dialog over the workbook can stay open then, reading
/// what it shows again; any other change (rows or columns moved, a sheet
/// removed or renamed, a workbook opened, an import) may move what it
/// shows.
pub fn keeps_cells_in_place(verb: &str) -> bool {
    matches!(
        verb,
        "cell.set"
            | "range.set"
            | "range.clear"
            | "cell.format"
            | "col.width"
            | "comment.add"
            | "wb.replace-all"
            | "sheet.add"
            | "table.rename"
    )
}

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
        "sheet.import-text" => sheet_import_text(app, args),
        "range.text-to-columns" => range_text_to_columns(app, args),
        "app.options" => app_options(app, args),
        "wb.replace-all" => wb_replace_all(app, args),
        "wb.consolidate" => wb_consolidate(app, args),
        "sheet.add" => sheet_add(app, args),
        "sheet.remove" => sheet_remove(app, args),
        "sheet.rename" => sheet_rename(app, args),
        "table.list" => Ok(table_list(app)),
        "table.rename" => table_rename(app, args),
        "table.resize" => table_resize(app, args),
        "table.convert" => table_convert(app, args),
        "row.insert" => row_op(app, args, true),
        "row.delete" => row_op(app, args, false),
        "col.insert" => col_op(app, args, true),
        "col.delete" => col_op(app, args, false),
        "cell.format" => cell_format(app, args),
        "col.width" => col_width(app, args),
        "page.setup" => page_setup(app, args),
        "page.header" => page_header(app, args),
        "print-area.set" => print_area_op(app, args, "set"),
        "print-area.add" => print_area_op(app, args, "add"),
        "print-area.clear" => print_area_op(app, args, "clear"),
        "print-titles.set" => print_titles_set(app, args),
        "page-break.insert" => page_break_op(app, args, "insert"),
        "page-break.remove" => page_break_op(app, args, "remove"),
        "page-break.reset" => page_break_op(app, args, "reset"),
        "print.pages" => print_pages(app, args),
        "wb.export-pdf" => wb_export_pdf(app, args),
        "filter.set" => datacmds::filter_set(app, args),
        "filter.reapply" => datacmds::filter_reapply(app, args),
        "filter.clear" => datacmds::filter_clear(app, args),
        "filter.off" => datacmds::filter_off(app, args),
        "filter.menu" => datacmds::filter_menu(app, args),
        "filter.by-cell" => datacmds::filter_by_cell(app, args),
        "filter.advanced" => datacmds::filter_advanced(app, args),
        "sheet.rows" => datacmds::sheet_rows(app, args),
        "range.sort" => datacmds::range_sort(app, args),
        "wb.clock" => datacmds::wb_clock(app, args),
        "wb.properties" => Ok(properties_json(app)),
        "wb.set-properties" => set_properties(app, args),
        "wb.recalc" => {
            app.recalc_and_refresh();
            Ok(Json::obj(vec![("recalculated", Json::Bool(true))]))
        }
        "wb.save" => {
            app.save_current()?;
            Ok(path_info(app))
        }
        "wb.reload" => {
            app.reload()?;
            Ok(path_info(app))
        }
        "wb.open" => {
            let p = args
                .get_str("path")
                .ok_or("wb.open needs a 'path' string")?
                .to_string();
            app.open_without_wizard(&p)?;
            Ok(path_info(app))
        }
        other => Err(format!("unknown verb '{other}'")),
    };
    if out.is_ok() {
        // An agent edit flashes this pane's status dot, so a watcher sees the
        // workbook being worked on.
        if edits(verb) {
            ctlcore::signal_activity();
        }
        // The filter verbs and `range.sort` signal themselves, only when
        // they changed something (their replies say `changed`): Clear with
        // nothing filtered, a Sort Warning reply, or a sort that left every
        // row in place moved nothing.
        // `comment.remove` can legitimately no-op (nothing on the cell), so it
        // signals itself inside `comment_remove`, gated on `removed:true` — a
        // no-op must not flash the activity dot (docxy's no-op principle).
        // `wb.set-properties` does the same, gated on `changed:true`, and so
        // do the page-layout verbs (`page.*`, `print-area.*`,
        // `print-titles.set`, `page-break.*`).
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
        // Bound to the file `xlsxy --read-only` opened (#882): `wb.save`
        // is refused until a Save As to another name.
        ("read_only", Json::Bool(app.bound_read_only())),
        ("sheets", Json::Num(wb.sheets.len() as f64)),
        ("active", Json::Num(app.sheet as f64)),
        ("active_name", Json::Str(wb.sheets[app.sheet].name.clone())),
        (
            "circular",
            Json::Arr(app.circular_refs().into_iter().map(Json::Str).collect()),
        ),
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

/// `wb.properties`: the document properties File › Info shows.
fn properties_json(app: &App) -> Json {
    let p = app.pkg.doc_properties();
    let opt = |v: &Option<String>| v.clone().map(Json::Str).unwrap_or(Json::Null);
    let custom = p
        .custom
        .iter()
        .map(|c| {
            let value = match &c.value {
                CustomValue::Number(n) => Json::Num(*n),
                CustomValue::Bool(b) => Json::Bool(*b),
                CustomValue::Text(s) | CustomValue::Date(s) | CustomValue::Other(s) => {
                    Json::Str(s.clone())
                }
            };
            Json::obj(vec![
                ("name", Json::Str(c.name.clone())),
                ("type", Json::Str(c.value.type_name().into())),
                ("value", value),
            ])
        })
        .collect();
    Json::obj(vec![
        ("title", opt(&p.title)),
        ("tags", opt(&p.keywords)),
        ("categories", opt(&p.category)),
        ("subject", opt(&p.subject)),
        ("comments", opt(&p.description)),
        ("company", opt(&p.company)),
        ("manager", opt(&p.manager)),
        ("hyperlinkBase", opt(&p.hyperlink_base)),
        ("author", opt(&p.creator)),
        ("lastModifiedBy", opt(&p.last_modified_by)),
        ("created", opt(&p.created)),
        ("modified", opt(&p.modified)),
        ("custom", Json::Arr(custom)),
    ])
}

/// `wb.set-properties`: set the given properties (`null`/`""` removes one).
/// Everything is checked before anything changes.
fn set_properties(app: &mut App, args: &Json) -> Result<Json, String> {
    let Json::Obj(pairs) = args else {
        return Err("wb.set-properties needs an object of properties".into());
    };
    let before = app.pkg.doc_properties();
    let mut p = before.clone();
    for (key, value) in pairs {
        let slot = match key.as_str() {
            "title" => &mut p.title,
            "tags" => &mut p.keywords,
            "categories" => &mut p.category,
            "subject" => &mut p.subject,
            "comments" => &mut p.description,
            "company" => &mut p.company,
            "manager" => &mut p.manager,
            "hyperlinkBase" => &mut p.hyperlink_base,
            "custom" => {
                set_custom_properties(&mut p.custom, value)?;
                continue;
            }
            // The MCP bridge forwards its instance selector with the args.
            "target" => continue,
            other => return Err(format!("wb.set-properties: unknown property '{other}'")),
        };
        *slot = match value {
            Json::Null => None,
            Json::Str(s) if s.is_empty() => None,
            Json::Str(s) => Some(s.clone()),
            _ => {
                return Err(format!(
                    "wb.set-properties: '{key}' must be a string or null"
                ));
            }
        };
    }
    let changed = p != before;
    if changed {
        app.pkg.set_doc_properties(&p)?;
        app.modified = true;
        ctlcore::signal_activity();
    }
    let mut out = properties_json(app);
    if let Json::Obj(fields) = &mut out {
        fields.push(("changed".into(), Json::Bool(changed)));
    }
    Ok(out)
}

/// Apply `{name: value|null}` to the custom list: a known name (any case)
/// changes in place, a new one is appended, `null`/`""` removes.
fn set_custom_properties(custom: &mut Vec<CustomProperty>, edits: &Json) -> Result<(), String> {
    let Json::Obj(edits) = edits else {
        return Err("wb.set-properties: 'custom' must be an object of name: value".into());
    };
    for (name, value) in edits {
        let name = name.trim();
        if name.is_empty() {
            return Err("wb.set-properties: a custom property needs a name".into());
        }
        let value = match value {
            Json::Null => None,
            Json::Str(s) if s.is_empty() => None,
            Json::Str(s) => Some(CustomValue::Text(s.clone())),
            Json::Num(n) if n.is_finite() => Some(CustomValue::Number(*n)),
            Json::Bool(b) => Some(CustomValue::Bool(*b)),
            Json::Obj(_) => {
                let date = value
                    .get_str("date")
                    .and_then(gridcore::docprops::normalize_date)
                    .ok_or_else(|| {
                        format!(
                            "wb.set-properties: custom '{name}' date must be {{\"date\":\"YYYY-MM-DD[THH:MM:SSZ]\"}}"
                        )
                    })?;
                Some(CustomValue::Date(date))
            }
            _ => {
                return Err(format!(
                    "wb.set-properties: custom '{name}' must be a string, number, bool, {{\"date\":…}} or null"
                ));
            }
        };
        let at = custom
            .iter()
            .position(|c| c.name.to_lowercase() == name.to_lowercase());
        match (at, value) {
            (Some(i), Some(v)) => custom[i].value = v,
            (Some(i), None) => {
                custom.remove(i);
            }
            (None, Some(v)) => custom.push(CustomProperty {
                name: name.to_string(),
                value: v,
            }),
            (None, None) => {}
        }
    }
    Ok(())
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
    // before it touches the workbook (a Text cell stores `=…` as text).
    if let Some(body) = gridcore::entry::typed_formula(&app.pkg.workbook, si, r, c, text) {
        Engine::validate(body).map_err(|e| format!("formula error: {e}"))?;
    }
    // Typed the way the grid types it: the cell's format decides, a
    // recognised shape takes its format, and an over-long entry is refused.
    let cell = gridcore::entry::entry_cell(&mut app.pkg.workbook, si, r, c, text, now_serial())
        .map_err(|e| format!("cell.set: {e}"))?;
    if !app.apply_on(si, vec![(r, c, cell)]) {
        return Err(format!("cell.set: {PART_OF_ARRAY}"));
    }
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
    if !app.apply_on(si, changes) {
        return Err(format!("range.clear: {PART_OF_ARRAY}"));
    }
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
    if !app
        .pkg
        .add_threaded_comment(si, r, c, &author, text, &iso_now())
    {
        return Err("comment.add: this sheet's XML is damaged where the comment would go".into());
    }
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

/// Write a rectangular block of cells starting at `start`, each string typed
/// the way `cell.set` types it (gridcore::entry), atomically: every formula
/// and every length in the batch is checked *before* anything is applied, so
/// a bad formula or an over-long entry anywhere in the block leaves the sheet
/// (and the undo stack) completely untouched. The whole block lands as one
/// [`App::apply_on`] call, i.e. one undo group.
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

    // Pass 1: validate every formula and length before touching anything
    // (atomicity).
    for (r, c, text) in &entries {
        gridcore::entry::check_len(text)
            .map_err(|e| format!("range.set: {} at {}", e, cell_name(*r, *c)))?;
        if let Some(body) = gridcore::entry::typed_formula(&app.pkg.workbook, si, *r, *c, text) {
            Engine::validate(body)
                .map_err(|e| format!("range.set: formula error at {}: {e}", cell_name(*r, *c)))?;
        }
    }

    // Pass 2: every entry validated — build the changes and apply as one group.
    let today = now_serial();
    let mut changes: Vec<(u32, u32, Cell)> = Vec::with_capacity(entries.len());
    for (r, c, text) in entries {
        let cell = gridcore::entry::entry_cell(&mut app.pkg.workbook, si, r, c, &text, today)
            .map_err(|e| format!("range.set: {} at {}", e, cell_name(r, c)))?;
        changes.push((r, c, cell));
    }
    let n = changes.len();
    if !app.apply_on(si, changes) {
        return Err(format!("range.set: {PART_OF_ARRAY}"));
    }
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

/// The Text Import Wizard / Text to Columns options of a verb's `options`
/// object, and the file origin. Every key is optional; the defaults are the
/// wizard's (delimited by Tab, `"` qualifier, row 1, General columns).
///
/// `kind` (`delimited`|`fixed`), `delimiters` (an array of `tab`,
/// `semicolon`, `comma`, `space` or any single character), `consecutive`,
/// `qualifier` (`"`, `'` or `none`), `breaks` (fixed-width positions),
/// `start_row` (1-based), `origin` (`auto`, `utf-8`, `utf-16le`,
/// `windows-1252`), `columns` (`general`, `text`, `date:dmy`…, `skip`),
/// `decimal`, `thousands` (one character each) and `trailing_minus`.
fn text_options(
    args: &Json,
) -> Result<(gridcore::textio::TextParse, gridcore::textio::Origin), String> {
    use gridcore::textio::{ColFormat, Delimiters, Origin, SplitKind, TextParse};
    let mut opts = TextParse::default();
    let mut origin = Origin::Auto;
    let o = match args.get("options") {
        None | Some(Json::Null) => return Ok((opts, origin)),
        Some(o @ Json::Obj(_)) => o,
        Some(_) => return Err("'options' must be an object".into()),
    };
    let one_char = |key: &str| -> Result<Option<char>, String> {
        match o.get_str(key) {
            None => Ok(None),
            Some(v) => {
                let mut cs = v.chars();
                match (cs.next(), cs.next()) {
                    (Some(c), None) => Ok(Some(c)),
                    _ => Err(format!("'{key}' must be one character")),
                }
            }
        }
    };
    let flag = |key: &str| o.get(key).and_then(Json::as_bool);
    let fixed = match o.get_str("kind") {
        None => o.get("breaks").is_some(),
        Some("delimited") => false,
        Some("fixed") => true,
        Some(k) => return Err(format!("unknown kind '{k}' (delimited or fixed)")),
    };
    if fixed {
        let breaks = match o.get("breaks") {
            None => Vec::new(),
            Some(b) => b
                .as_array()
                .ok_or("'breaks' must be an array of positions")?
                .iter()
                .map(|v| v.as_usize().ok_or("'breaks' must be an array of positions"))
                .collect::<Result<_, _>>()?,
        };
        opts.kind = SplitKind::Fixed { breaks };
    } else {
        let mut delims = Delimiters::only('\t');
        if let Some(list) = o.get("delimiters") {
            delims = Delimiters::default();
            let list = list.as_array().ok_or("'delimiters' must be an array")?;
            for d in list {
                let d = d.as_str().ok_or("'delimiters' must be strings")?;
                match d.to_ascii_lowercase().as_str() {
                    "tab" | "\t" => delims.tab = true,
                    "semicolon" | ";" => delims.semicolon = true,
                    "comma" | "," => delims.comma = true,
                    "space" | " " => delims.space = true,
                    _ => {
                        let mut cs = d.chars();
                        match (cs.next(), cs.next()) {
                            (Some(c), None) => delims.other = Some(c),
                            _ => return Err(format!("unknown delimiter '{d}'")),
                        }
                    }
                }
            }
        }
        opts.kind = SplitKind::Delimited {
            delims,
            consecutive: flag("consecutive").unwrap_or(false),
        };
    }
    if let Some(q) = o.get_str("qualifier") {
        opts.qualifier = match q {
            "none" | "" => None,
            "\"" | "'" => q.chars().next(),
            _ => return Err(format!("unknown qualifier '{q}' (\", ' or none)")),
        };
    }
    if let Some(v) = o.get("start_row") {
        opts.start_row = v
            .as_usize()
            .filter(|n| *n >= 1)
            .ok_or("'start_row' must be a row number from 1")?;
    }
    if let Some(v) = o.get_str("origin") {
        origin = Origin::parse(v).ok_or_else(|| format!("unknown origin '{v}'"))?;
    }
    if let Some(cols) = o.get("columns") {
        opts.columns = cols
            .as_array()
            .ok_or("'columns' must be an array")?
            .iter()
            .map(|c| {
                c.as_str()
                    .and_then(ColFormat::parse)
                    .ok_or_else(|| format!("unknown column format {c:?}"))
            })
            .collect::<Result<_, _>>()?;
    }
    if let Some(c) = one_char("decimal")? {
        opts.decimal = c;
    }
    if let Some(c) = one_char("thousands")? {
        opts.thousands = c;
    }
    if opts.decimal == opts.thousands {
        return Err("the decimal and thousands separators must differ".into());
    }
    if let Some(b) = flag("trailing_minus") {
        opts.trailing_minus = b;
    }
    Ok((opts, origin))
}

/// File › Options: Data › Automatic Data Conversion's four switches and
/// Advanced › Editing's options (#672). Every key given is checked before
/// any is set, so a bad one changes nothing; the reply holds every value,
/// set or not. The conversion switches apply to the next `.csv`/text open;
/// all of them are saved with the app's preferences on exit.
fn app_options(app: &mut App, args: &Json) -> Result<Json, String> {
    use gridcore::options::{self as o, EnterMove};
    const CONVERT: [&str; 4] = crate::CONVERT_KEYS;
    const EDIT_FLAGS: [&str; 4] = [
        o::KEY_FIXED_DECIMAL,
        o::KEY_MOVE_AFTER_ENTER,
        o::KEY_EDIT_IN_CELL,
        o::KEY_AUTOCOMPLETE,
    ];
    let given = |key: &str| args.get(key).filter(|v| **v != Json::Null);
    let flag = |key: &str| match given(key) {
        None => Ok(None),
        Some(Json::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("'{key}' must be true or false")),
    };
    // Every key is checked before any is set: a bad one changes nothing.
    let mut convert = [None; 4];
    for (slot, key) in convert.iter_mut().zip(CONVERT) {
        *slot = flag(key)?;
    }
    let mut edit = [None; 4];
    for (slot, key) in edit.iter_mut().zip(EDIT_FLAGS) {
        *slot = flag(key)?;
    }
    let places = match given(o::KEY_PLACES) {
        None => None,
        Some(Json::Num(n))
            if n.fract() == 0.0
                && (f64::from(o::PLACES_MIN)..=f64::from(o::PLACES_MAX)).contains(n) =>
        {
            Some(*n as i16)
        }
        Some(_) => {
            return Err(format!(
                "'{}' must be a whole number from {} to {}",
                o::KEY_PLACES,
                o::PLACES_MIN,
                o::PLACES_MAX
            ));
        }
    };
    let direction = match given(o::KEY_MOVE_DIRECTION) {
        None => None,
        Some(Json::Str(s)) if EnterMove::from_label(s).is_some() => EnterMove::from_label(s),
        Some(_) => {
            return Err(format!(
                "'{}' must be down, right, up or left",
                o::KEY_MOVE_DIRECTION
            ));
        }
    };
    let auto = &mut app.auto_convert;
    let fields: [&mut bool; 4] = [
        &mut auto.remove_leading_zeros,
        &mut auto.keep_15_digits,
        &mut auto.e_notation,
        &mut auto.dates,
    ];
    let mut out = Vec::new();
    for ((key, slot), want) in CONVERT.into_iter().zip(fields).zip(convert) {
        if let Some(b) = want {
            *slot = b;
        }
        out.push((key, Json::Bool(*slot)));
    }
    let e = &mut app.edit_opts;
    let fields: [&mut bool; 4] = [
        &mut e.fixed_decimal,
        &mut e.move_after_enter,
        &mut e.edit_in_cell,
        &mut e.autocomplete,
    ];
    for ((key, slot), want) in EDIT_FLAGS.into_iter().zip(fields).zip(edit) {
        if let Some(b) = want {
            *slot = b;
        }
        out.push((key, Json::Bool(*slot)));
    }
    if let Some(p) = places {
        e.places = p;
    }
    if let Some(d) = direction {
        e.enter_move = d;
    }
    out.push((o::KEY_PLACES, Json::Num(f64::from(e.places))));
    out.push((
        o::KEY_MOVE_DIRECTION,
        Json::Str(e.enter_move.label().to_ascii_lowercase()),
    ));
    Ok(Json::obj(out))
}

/// The Text Import Wizard without the dialog: `text` (or the file at
/// `path`) read under `options` into a brand-new sheet, as `sheet.import-csv`
/// adds one.
fn sheet_import_text(app: &mut App, args: &Json) -> Result<Json, String> {
    let (opts, origin) = text_options(args)?;
    let text = match (args.get_str("text"), args.get_str("path")) {
        (Some(t), _) => t.to_string(),
        (None, Some(p)) => {
            let bytes = std::fs::read(p).map_err(|e| format!("cannot read {p}: {e}"))?;
            gridcore::textio::decode(&bytes, origin)
        }
        (None, None) => return Err("sheet.import-text needs 'text' or 'path'".into()),
    };
    import_new_sheet(app, args, &text, &opts)
}

/// Data › Text to Columns on one column: `range` converted under `options`
/// into `dest` (default: the range's first cell). When that overwrites cells
/// holding data the verb refuses with Excel's question unless `replace` is
/// true. One undo step.
fn range_text_to_columns(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let rg = args
        .get_str("range")
        .ok_or("range.text-to-columns needs a 'range'")?;
    let range = parse_range_name(rg.trim()).ok_or_else(|| format!("bad range '{rg}'"))?;
    let mut src = gridcore::edit::TtcSource::new(si, range).map_err(str::to_string)?;
    if let Some(d) = args.get_str("dest") {
        src.dest = parse_cell_name(d.trim()).ok_or_else(|| format!("bad cell ref '{d}'"))?;
    }
    let (opts, _) = text_options(args)?;
    let replace = args.get("replace").and_then(Json::as_bool).unwrap_or(false);
    if !replace && gridcore::edit::ttc_would_overwrite(&app.pkg.workbook, &src, &opts) {
        return Err(format!(
            "{} (pass replace:true to overwrite)",
            gridcore::edit::TTC_REPLACE
        ));
    }
    let rows = app.apply_text_to_columns(&src, &opts);
    Ok(Json::obj(vec![("rows", Json::Num(rows as f64))]))
}

/// Data › Consolidate, as the dialog's OK does it.
fn wb_consolidate(app: &mut App, args: &Json) -> Result<Json, String> {
    let refs = args
        .get("refs")
        .and_then(Json::as_array)
        .ok_or("wb.consolidate needs 'refs' (an array of references)")?
        .iter()
        .map(|r| {
            r.as_str()
                .map(str::to_string)
                .ok_or("each ref must be a string")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let func = match args.get_str("fn") {
        Some(f) => gridcore::edit::parse_consolidate_func(f)
            .ok_or_else(|| format!("unknown function '{f}'"))?,
        None => Default::default(),
    };
    let (sheet, at) = match args.get_str("dest") {
        Some(d) => {
            let r = gridcore::edit::parse_consolidate_ref(&app.pkg.workbook, app.sheet, d)
                .map_err(|_| format!("bad destination '{d}'"))?;
            let ix = app.pkg.workbook.sheet_index(&r.sheet).unwrap_or(app.sheet);
            (ix, (r.area.0, r.area.1))
        }
        None => (app.sheet, app.cur),
    };
    let flag = |k: &str| args.get(k).and_then(Json::as_bool).unwrap_or(false);
    let opts = gridcore::edit::ConsolidateOptions {
        func,
        refs,
        top_row: flag("top"),
        left_col: flag("left"),
        links: flag("links"),
        book_name: String::new(),
    };
    let (r1, c1, r2, c2) = app.apply_consolidate(sheet, at, opts)?;
    Ok(Json::obj(vec![
        (
            "sheet",
            Json::Str(app.pkg.workbook.sheets[sheet].name.clone()),
        ),
        (
            "range",
            Json::Str(format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))),
        ),
    ]))
}

/// Import CSV text as a brand-new sheet (never overwrites an existing one —
/// name collisions are deduplicated), converted exactly as opening a `.csv`
/// converts it (`csv_to_pkg`), but into a sheet of the *live* workbook.
fn sheet_import_csv(app: &mut App, args: &Json) -> Result<Json, String> {
    let text = args
        .get_str("text")
        .ok_or("sheet.import-csv needs 'text'")?;
    let (opts, body) = super::csv_parse(text, false);
    import_new_sheet(app, args, body, &opts)
}

/// `text` read under `opts` into a brand-new sheet named by `args.name`
/// (deduplicated), converted as opening a text file converts it. Shared by
/// `sheet.import-csv` and `sheet.import-text`.
fn import_new_sheet(
    app: &mut App,
    args: &Json,
    text: &str,
    opts: &gridcore::textio::TextParse,
) -> Result<Json, String> {
    let requested = args.get_str("name").unwrap_or("Sheet");
    let name = unique_sheet_name(&app.pkg.workbook, requested);
    let idx = app.pkg.add_sheet(&name);
    let open = app.text_open();
    let wb = &mut app.pkg.workbook;
    let date1904 = wb.date1904;
    super::import_text(
        &mut wb.sheets[idx],
        &mut wb.styles,
        text,
        opts,
        &open,
        date1904,
    );
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
/// through [`App::structural_writing_cells`] (not `apply_on`) so the whole
/// multi-sheet edit lands as a single undo group, a rewritten table header
/// renames its column, and the engine is rebuilt and recalculated afterward.
fn wb_replace_all(app: &mut App, args: &Json) -> Result<Json, String> {
    let query = args
        .get_str("query")
        .ok_or("wb.replace-all needs a 'query'")?;
    if query.is_empty() {
        return Err("empty query".into());
    }
    let text = args.get_str("text").ok_or("wb.replace-all needs 'text'")?;
    let mut replaced = 0usize;
    let today = now_serial();
    app.structural_writing_cells(|wb| {
        let ctx = gridcore::entry::entry_ctx(wb, today);
        for sheet in &mut wb.sheets {
            let changes =
                gridcore::edit::replace_all_in_sheet(sheet, &mut wb.styles, &ctx, query, text);
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
    // A pending cut's source sheet may be gone or renumbered.
    app.cancel_cut();
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

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

fn table_json(app: &App, t: &gridcore::sheet::Table) -> Json {
    let (r1, c1, r2, c2) = t.range;
    Json::obj(vec![
        ("name", Json::Str(t.name.clone())),
        ("sheet", Json::Num(t.sheet as f64)),
        (
            "sheet_name",
            Json::Str(app.pkg.workbook.sheets[t.sheet].name.clone()),
        ),
        (
            "ref",
            Json::Str(format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))),
        ),
        (
            "columns",
            Json::Arr(t.columns.iter().cloned().map(Json::Str).collect()),
        ),
        ("header_rows", Json::Num(t.header_rows as f64)),
        ("totals_rows", Json::Num(t.totals_rows as f64)),
    ])
}

fn table_list(app: &App) -> Json {
    let tables = app
        .pkg
        .workbook
        .tables
        .iter()
        .map(|t| table_json(app, t))
        .collect();
    Json::obj(vec![("tables", Json::Arr(tables))])
}

/// The table named by `name` in `args` (as the workbook spells it).
fn table_arg(app: &App, args: &Json, verb: &str) -> Result<String, String> {
    let name = args
        .get_str("name")
        .ok_or_else(|| format!("{verb} needs a 'name'"))?;
    app.pkg
        .workbook
        .table(name)
        .map(|t| t.name.clone())
        .ok_or_else(|| format!("There is no table named {name}"))
}

/// The table now named `name`, as `table.list` shows it.
fn table_reply(app: &App, name: &str) -> Result<Json, String> {
    let t = app.pkg.workbook.table(name).ok_or("table vanished")?;
    Ok(table_json(app, t))
}

/// Table Name — [`App::rename_table`]: one undo step.
fn table_rename(app: &mut App, args: &Json) -> Result<Json, String> {
    let name = table_arg(app, args, "table.rename")?;
    let new = args
        .get_str("new")
        .ok_or("table.rename needs a 'new' name")?;
    app.rename_table(&name, new)?;
    table_reply(app, new)
}

/// Resize Table — [`App::resize_table`]: one undo step.
fn table_resize(app: &mut App, args: &Json) -> Result<Json, String> {
    let name = table_arg(app, args, "table.resize")?;
    let range = args.get_str("ref").ok_or("table.resize needs a 'ref'")?;
    app.resize_table(&name, range)?;
    table_reply(app, &name)
}

/// Convert to Range — [`App::convert_table`]: one undo step.
fn table_convert(app: &mut App, args: &Json) -> Result<Json, String> {
    let name = table_arg(app, args, "table.convert")?;
    app.convert_table(&name)?;
    Ok(Json::obj(vec![("converted", Json::Str(name))]))
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
/// `Styles::intern`/`apply_format` path — one [`App::apply_styles_on`] call,
/// so the whole range lands as ONE undo group exactly like the TUI's own
/// `apply_format`. Only each cell's style index changes: value, formula and
/// spill are never re-entered, so a spilled block stays spilled.
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

    let snapshot: Vec<(u32, u32, u32)> = {
        let sheet = &app.pkg.workbook.sheets[si];
        let mut v = Vec::new();
        for r in r1..=r2 {
            for c in c1..=c2 {
                v.push((r, c, sheet.cell(r, c).map_or(0, |cl| cl.style)));
            }
        }
        v
    };
    let mut styles = Vec::with_capacity(snapshot.len());
    for (r, c, cur) in snapshot {
        let base_xf = app.pkg.workbook.styles.xf(cur);
        let new_xf = apply_patch_to_xf(&base_xf, &patch);
        let idx = app.pkg.workbook.styles.intern(new_xf);
        styles.push((r, c, idx));
    }
    let formatted = styles.len();
    app.apply_styles_on(si, styles);
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
// Page layout and printing
// ---------------------------------------------------------------------------

/// The sheets a page-layout verb applies to: `sheets` (indexes or names),
/// else `sheet`, else the active one.
/// A sheet named twice (`[0, 0]`, `["Sheet1", "sheet1"]`) is one sheet,
/// listed once, in its first place.
fn sheets_arg(app: &App, args: &Json) -> Result<Vec<usize>, String> {
    match args.get("sheets") {
        None | Some(Json::Null) => Ok(vec![sheet_arg(app, args)?]),
        Some(Json::Arr(items)) if !items.is_empty() => {
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for v in items {
                let si = sheet_arg(app, &Json::obj(vec![("sheet", v.clone())]))?;
                if seen.insert(si) {
                    out.push(si);
                }
            }
            Ok(out)
        }
        Some(_) => Err("'sheets' must be a non-empty array of indexes or names".into()),
    }
}

/// A `u32` field: a whole number ≥ 0.
fn u32_field(verb: &str, key: &str, v: &Json) -> Result<u32, String> {
    match v.as_f64() {
        Some(n) if n >= 0.0 && n.fract() == 0.0 && n <= f64::from(u32::MAX) => Ok(n as u32),
        _ => Err(format!("{verb}: '{key}' must be a whole number ≥ 0")),
    }
}

fn bool_field(verb: &str, key: &str, v: &Json) -> Result<bool, String> {
    v.as_bool()
        .ok_or_else(|| format!("{verb}: '{key}' must be true or false"))
}

fn word_field<'a>(verb: &str, key: &str, v: &'a Json) -> Result<&'a str, String> {
    v.as_str()
        .ok_or_else(|| format!("{verb}: '{key}' must be a string"))
}

/// Apply `page.setup`'s field keys to `s`. As in Excel's Page Setup, setting
/// a fit count turns `Fit to` on and setting `scale` turns it off, unless
/// `fitToPage` itself is given.
fn apply_page_fields(s: &mut PageSetup, args: &Json) -> Result<(), String> {
    const VERB: &str = "page.setup";
    let Json::Obj(pairs) = args else {
        return Ok(());
    };
    for (key, v) in pairs {
        let k = key.as_str();
        match k {
            // Addressing, not fields: `target` picks the editor over MCP.
            "sheet" | "sheets" | "target" => {}
            "margins" => {
                let Json::Obj(m) = v else {
                    return Err(format!("{VERB}: 'margins' must be an object of inches"));
                };
                for (side, inches) in m {
                    let n = inches
                        .as_f64()
                        .ok_or_else(|| format!("{VERB}: margin '{side}' must be a number"))?;
                    let field = match side.as_str() {
                        "left" => &mut s.margins.left,
                        "right" => &mut s.margins.right,
                        "top" => &mut s.margins.top,
                        "bottom" => &mut s.margins.bottom,
                        "header" => &mut s.margins.header,
                        "footer" => &mut s.margins.footer,
                        other => return Err(format!("{VERB}: unknown margin '{other}'")),
                    };
                    *field = n;
                }
            }
            "paperSize" => s.paper_size = u32_field(VERB, k, v)?,
            "orientation" => {
                let w = word_field(VERB, k, v)?;
                s.orientation = Orientation::parse(w).ok_or_else(|| {
                    format!("{VERB}: orientation is portrait, landscape or default, not '{w}'")
                })?;
            }
            "scale" => {
                s.scale = u32_field(VERB, k, v)?;
                if args.get("fitToPage").is_none() {
                    s.fit_to_page = false;
                }
            }
            "fitToPage" => s.fit_to_page = bool_field(VERB, k, v)?,
            "fitToWidth" | "fitToHeight" => {
                let n = u32_field(VERB, k, v)?;
                if k == "fitToWidth" {
                    s.fit_width = n;
                } else {
                    s.fit_height = n;
                }
                if args.get("fitToPage").is_none() {
                    s.fit_to_page = true;
                }
            }
            "firstPageNumber" => {
                s.first_page_number = match v {
                    Json::Null => None,
                    v => Some(u32_field(VERB, k, v)?),
                }
            }
            "pageOrder" => {
                let w = word_field(VERB, k, v)?;
                s.page_order = PageOrder::parse(w).ok_or_else(|| {
                    format!("{VERB}: pageOrder is downThenOver or overThenDown, not '{w}'")
                })?;
            }
            "cellComments" => {
                let w = word_field(VERB, k, v)?;
                s.cell_comments = CellComments::parse(w).ok_or_else(|| {
                    format!("{VERB}: cellComments is none, asDisplayed or atEnd, not '{w}'")
                })?;
            }
            "errors" => {
                let w = word_field(VERB, k, v)?;
                s.errors = PrintErrors::parse(w).ok_or_else(|| {
                    format!("{VERB}: errors is displayed, blank, dash or NA, not '{w}'")
                })?;
            }
            "blackAndWhite" => s.black_and_white = bool_field(VERB, k, v)?,
            "draft" => s.draft = bool_field(VERB, k, v)?,
            "gridLines" => s.grid_lines = bool_field(VERB, k, v)?,
            "headings" => s.headings = bool_field(VERB, k, v)?,
            "horizontalCentered" => s.h_centered = bool_field(VERB, k, v)?,
            "verticalCentered" => s.v_centered = bool_field(VERB, k, v)?,
            "differentOddEven" => s.header_footer.different_odd_even = bool_field(VERB, k, v)?,
            "differentFirst" => s.header_footer.different_first = bool_field(VERB, k, v)?,
            "scaleWithDoc" => s.header_footer.scale_with_doc = bool_field(VERB, k, v)?,
            "alignWithMargins" => s.header_footer.align_with_margins = bool_field(VERB, k, v)?,
            other => return Err(format!("{VERB}: unknown field '{other}'")),
        }
    }
    s.validate().map_err(|e| format!("{VERB}: {e}"))
}

/// Does `page.setup` carry any field to set?
fn has_page_fields(args: &Json) -> bool {
    match args {
        Json::Obj(pairs) => pairs
            .iter()
            .any(|(k, _)| !matches!(k.as_str(), "sheet" | "sheets" | "target")),
        _ => false,
    }
}

fn opt_str(s: Option<&str>) -> Json {
    s.map_or(Json::Null, |s| Json::Str(s.to_string()))
}

/// A sheet's page layout as `page.setup` reports it.
fn page_setup_json(app: &App, si: usize) -> Json {
    let wb = &app.pkg.workbook;
    let sheet = &wb.sheets[si];
    let s = &sheet.page_setup;
    let hf = &s.header_footer;
    let m = &s.margins;
    let titles = area::print_titles(wb, si);
    let span = |v: Option<(u32, u32)>, rows: bool| match v {
        Some((a, b)) if rows => Json::Str(format!("{}:{}", a + 1, b + 1)),
        Some((a, b)) => Json::Str(format!("{}:{}", col_name(a), col_name(b))),
        None => Json::Null,
    };
    let print_area: Vec<String> = area::print_area(wb, si)
        .into_iter()
        .map(area::rect_name)
        .collect();
    let (row_breaks, col_breaks) = area::manual_breaks(sheet);
    let ids = |v: Vec<u32>| Json::Arr(v.into_iter().map(|i| Json::Num(i as f64)).collect());
    Json::obj(vec![
        ("sheet", Json::Num(si as f64)),
        ("name", Json::Str(sheet.name.clone())),
        (
            "margins",
            Json::obj(vec![
                ("left", Json::Num(m.left)),
                ("right", Json::Num(m.right)),
                ("top", Json::Num(m.top)),
                ("bottom", Json::Num(m.bottom)),
                ("header", Json::Num(m.header)),
                ("footer", Json::Num(m.footer)),
            ]),
        ),
        ("paperSize", Json::Num(s.paper_size as f64)),
        ("orientation", Json::Str(s.orientation.as_str().into())),
        ("scale", Json::Num(s.scale as f64)),
        ("fitToPage", Json::Bool(s.fit_to_page)),
        ("fitToWidth", Json::Num(s.fit_width as f64)),
        ("fitToHeight", Json::Num(s.fit_height as f64)),
        (
            "firstPageNumber",
            s.first_page_number
                .map_or(Json::Null, |n| Json::Num(n as f64)),
        ),
        ("pageOrder", Json::Str(s.page_order.as_str().into())),
        ("blackAndWhite", Json::Bool(s.black_and_white)),
        ("draft", Json::Bool(s.draft)),
        ("cellComments", Json::Str(s.cell_comments.as_str().into())),
        ("errors", Json::Str(s.errors.as_str().into())),
        ("gridLines", Json::Bool(s.grid_lines)),
        ("headings", Json::Bool(s.headings)),
        ("horizontalCentered", Json::Bool(s.h_centered)),
        ("verticalCentered", Json::Bool(s.v_centered)),
        ("differentOddEven", Json::Bool(hf.different_odd_even)),
        ("differentFirst", Json::Bool(hf.different_first)),
        ("scaleWithDoc", Json::Bool(hf.scale_with_doc)),
        ("alignWithMargins", Json::Bool(hf.align_with_margins)),
        (
            "headers",
            Json::Obj(
                HfSlot::ALL
                    .iter()
                    .map(|&slot| (slot.element().to_string(), opt_str(hf.get(slot))))
                    .collect(),
            ),
        ),
        (
            "printArea",
            if print_area.is_empty() {
                Json::Null
            } else {
                Json::Str(print_area.join(","))
            },
        ),
        (
            "printTitles",
            Json::obj(vec![
                ("rows", span(titles.rows, true)),
                ("cols", span(titles.cols, false)),
            ]),
        ),
        ("rowBreaks", ids(row_breaks)),
        ("colBreaks", ids(col_breaks)),
    ])
}

/// `page.setup`: read a sheet's page layout, or set fields of it. With
/// `sheets`, the fields go to the first sheet and its whole page setup is
/// then copied to the others, as Page Setup on grouped sheets does
/// (FIL-147): print areas and titles stay each sheet's own, and header
/// pictures aren't copied.
fn page_setup(app: &mut App, args: &Json) -> Result<Json, String> {
    // Deduplicated, so the first sheet is never its own group copy's target.
    let targets = sheets_arg(app, args)?;
    let first = targets[0];
    if !has_page_fields(args) {
        return Ok(page_setup_json(app, first));
    }
    let changed = app.layout_edit(|wb| {
        let mut s = wb.sheets[first].page_setup.clone();
        apply_page_fields(&mut s, args)?;
        let mut changed = s != wb.sheets[first].page_setup;
        let group = s.for_group();
        wb.sheets[first].page_setup = s;
        for &si in targets.iter().filter(|&&si| si != first) {
            if wb.sheets[si].page_setup != group {
                wb.sheets[si].page_setup = group.clone();
                changed = true;
            }
        }
        Ok(changed)
    })?;
    if changed {
        ctlcore::signal_activity();
    }
    Ok(with_changed(page_setup_json(app, first), changed))
}

/// `json` with `changed` added.
fn with_changed(json: Json, changed: bool) -> Json {
    match json {
        Json::Obj(mut pairs) => {
            pairs.push(("changed".into(), Json::Bool(changed)));
            Json::Obj(pairs)
        }
        other => other,
    }
}

/// `page.header`: read one header or footer as its three sections in the
/// editor's `&[…]` form, or set them. Field codes typed as `&[Page]` are
/// stored as Excel's `&P`; `&&` stays a literal ampersand, and `&L`, `&C`
/// or `&R` inside a section is refused (it would start another section). A
/// header picture (`&[Picture]`) can be kept where the section already
/// shows one, never added.
fn page_header(app: &mut App, args: &Json) -> Result<Json, String> {
    const VERB: &str = "page.header";
    let si = sheet_arg(app, args)?;
    let kind = args.get_str("kind").unwrap_or("odd");
    let part = args.get_str("part").unwrap_or("header");
    let slot = HfSlot::from_kind(kind, part).ok_or_else(|| {
        format!(
            "{VERB}: kind is odd, even or first and part is header or footer, not '{kind}'/'{part}'"
        )
    })?;
    let editing = ["left", "center", "right"]
        .iter()
        .any(|k| args.get(k).is_some());
    let mut changed = false;
    if editing {
        let text = |k: &str| -> Result<String, String> {
            match args.get(k) {
                None | Some(Json::Null) => Ok(String::new()),
                Some(Json::Str(s)) => Ok(s.clone()),
                Some(_) => Err(format!("{VERB}: '{k}' must be a string")),
            }
        };
        let sections = hf::Sections::from_editor(&text("left")?, &text("center")?, &text("right")?)
            .map_err(|e| format!("{VERB}: {e}"))?;
        let current = hf::Sections::parse(
            app.pkg.workbook.sheets[si]
                .page_setup
                .header_footer
                .get(slot)
                .unwrap_or(""),
        );
        let stored = sections.compose();
        // Checked on the string as it will be stored and read back, so no
        // spelling can move a picture into another section.
        let stored_as = hf::Sections::parse(&stored);
        for (name, new, old) in [
            ("left", &stored_as.left, &current.left),
            ("center", &stored_as.center, &current.center),
            ("right", &stored_as.right, &current.right),
        ] {
            if hf::has_code(new, 'G') && !hf::has_code(old, 'G') {
                return Err(format!(
                    "{VERB}: a header picture (&[Picture]) can only be kept where the {name} section already has one; inserting pictures is not supported"
                ));
            }
        }
        let new = (!stored.is_empty()).then_some(stored);
        changed = app.layout_edit(|wb| {
            let hf = &mut wb.sheets[si].page_setup.header_footer;
            let changed = *hf.slot_mut(slot) != new;
            *hf.slot_mut(slot) = new;
            Ok(changed)
        })?;
        if changed {
            ctlcore::signal_activity();
        }
    }
    let stored = app.pkg.workbook.sheets[si]
        .page_setup
        .header_footer
        .get(slot)
        .map(str::to_string);
    let s = hf::Sections::parse(stored.as_deref().unwrap_or(""));
    Ok(Json::obj(vec![
        ("sheet", Json::Num(si as f64)),
        ("kind", Json::Str(kind.into())),
        ("part", Json::Str(part.into())),
        ("stored", opt_str(stored.as_deref())),
        ("left", Json::Str(hf::decode(&s.left))),
        ("center", Json::Str(hf::decode(&s.center))),
        ("right", Json::Str(hf::decode(&s.right))),
        ("changed", Json::Bool(changed)),
    ]))
}

/// Ranges like `A1:C10,E1:F5`, `A:B` or `1:2`, every one readable.
fn ranges_arg(verb: &str, args: &Json, key: &str) -> Result<Vec<area::PrintRef>, String> {
    let text = args
        .get_str(key)
        .ok_or_else(|| format!("{verb} needs a '{key}' like \"A1:C10\""))?;
    let parts = text.split(',').filter(|p| !p.trim().is_empty()).count();
    let refs = area::parse_refs(text);
    if parts == 0 || refs.len() != parts {
        return Err(format!("{verb}: bad range '{text}'"));
    }
    Ok(refs)
}

fn print_area_json(app: &App, si: usize, changed: bool) -> Json {
    let wb = &app.pkg.workbook;
    let formula = wb
        .defined_names
        .iter()
        .find(|d| d.scope == Some(si) && d.name.eq_ignore_ascii_case(area::PRINT_AREA))
        .map(|d| d.formula.clone());
    Json::obj(vec![
        ("sheet", Json::Num(si as f64)),
        ("printArea", opt_str(formula.as_deref())),
        ("changed", Json::Bool(changed)),
    ])
}

/// `print-area.set` / `.add` / `.clear`.
fn print_area_op(app: &mut App, args: &Json, op: &str) -> Result<Json, String> {
    let verb = format!("print-area.{op}");
    let si = sheet_arg(app, args)?;
    let refs = match op {
        "clear" => Vec::new(),
        _ => ranges_arg(&verb, args, "range")?,
    };
    let changed = app.layout_edit(|wb| {
        let before = area::print_area(wb, si);
        match op {
            "set" => {
                let rects: Vec<_> = refs.iter().map(|r| r.rect()).collect();
                area::set_print_area(wb, si, &rects);
            }
            "add" => {
                for r in &refs {
                    area::add_print_area(wb, si, r.rect());
                }
            }
            _ => {
                area::clear_print_area(wb, si);
            }
        }
        Ok(area::print_area(wb, si) != before)
    })?;
    if changed {
        ctlcore::signal_activity();
    }
    Ok(print_area_json(app, si, changed))
}

/// `print-titles.set {rows?, cols?}`: `"1:2"` rows to repeat at top, `"A:A"`
/// columns at left. An absent key keeps that part; `null` or `""` clears it.
fn print_titles_set(app: &mut App, args: &Json) -> Result<Json, String> {
    const VERB: &str = "print-titles.set";
    let si = sheet_arg(app, args)?;
    let mut titles = area::print_titles(&app.pkg.workbook, si);
    for (key, rows) in [("rows", true), ("cols", false)] {
        match args.get(key) {
            None => {}
            Some(Json::Null) => set_titles_part(&mut titles, rows, None),
            Some(Json::Str(s)) if s.trim().is_empty() => set_titles_part(&mut titles, rows, None),
            Some(Json::Str(s)) => {
                let span = match area::parse_refs(s).as_slice() {
                    [area::PrintRef::Rows(a, b)] if rows => (*a, *b),
                    [area::PrintRef::Cols(a, b)] if !rows => (*a, *b),
                    _ => {
                        return Err(format!(
                            "{VERB}: '{key}' must be {} like \"{}\", not '{s}'",
                            if rows { "whole rows" } else { "whole columns" },
                            if rows { "1:2" } else { "A:B" }
                        ));
                    }
                };
                set_titles_part(&mut titles, rows, Some(span));
            }
            Some(_) => return Err(format!("{VERB}: '{key}' must be a string or null")),
        }
    }
    let changed = app.layout_edit(|wb| {
        let before = area::print_titles(wb, si);
        area::set_print_titles(wb, si, titles);
        Ok(area::print_titles(wb, si) != before)
    })?;
    if changed {
        ctlcore::signal_activity();
    }
    let json = page_setup_json(app, si);
    Ok(Json::obj(vec![
        ("sheet", Json::Num(si as f64)),
        (
            "printTitles",
            json.get("printTitles").cloned().unwrap_or(Json::Null),
        ),
        ("changed", Json::Bool(changed)),
    ]))
}

fn set_titles_part(t: &mut area::PrintTitles, rows: bool, span: Option<(u32, u32)>) {
    if rows {
        t.rows = span;
    } else {
        t.cols = span;
    }
}

/// `page-break.insert` / `.remove {cell}` / `.reset`.
fn page_break_op(app: &mut App, args: &Json, op: &str) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let cell = match op {
        "reset" => None,
        _ => {
            let r = args
                .get_str("cell")
                .ok_or_else(|| format!("page-break.{op} needs a 'cell' like \"A14\""))?;
            Some(parse_cell_name(r.trim()).ok_or_else(|| format!("bad cell ref '{r}'"))?)
        }
    };
    let changed = app.layout_edit(|wb| {
        let sheet = &mut wb.sheets[si];
        Ok(match (op, cell) {
            ("insert", Some((r, c))) => area::insert_page_break(sheet, r, c),
            ("remove", Some((r, c))) => area::remove_page_break(sheet, r, c),
            _ => area::reset_page_breaks(sheet),
        })
    })?;
    if changed {
        ctlcore::signal_activity();
    }
    let (rows, cols) = area::manual_breaks(&app.pkg.workbook.sheets[si]);
    let ids = |v: Vec<u32>| Json::Arr(v.into_iter().map(|i| Json::Num(i as f64)).collect());
    Ok(Json::obj(vec![
        ("sheet", Json::Num(si as f64)),
        ("rowBreaks", ids(rows)),
        ("colBreaks", ids(cols)),
        ("changed", Json::Bool(changed)),
    ]))
}

/// The print job `print.pages` and `wb.export-pdf` describe: `what` is
/// `active` (the default: `sheets`, or `sheet`, or the active sheet),
/// `workbook` (every visible sheet) or `selection` (`range`, comma-separated
/// ranges of `sheet`); `ignorePrintAreas`, `from` and `to` as on the Print
/// page.
fn job_arg(app: &App, verb: &str, args: &Json) -> Result<Job, String> {
    let what = match args.get_str("what").unwrap_or("active") {
        "active" => What::ActiveSheets(sheets_arg(app, args)?),
        "workbook" => What::EntireWorkbook,
        "selection" => What::Selection {
            sheet: sheet_arg(app, args)?,
            ranges: ranges_arg(verb, args, "range")?
                .into_iter()
                .map(|r| r.rect())
                .collect(),
        },
        other => {
            return Err(format!(
                "{verb}: what is active, workbook or selection, not '{other}'"
            ));
        }
    };
    let page = |key: &str| -> Result<Option<u32>, String> {
        match args.get(key) {
            None | Some(Json::Null) => Ok(None),
            Some(v) => match u32_field(verb, key, v)? {
                0 => Err(format!("{verb}: '{key}' counts pages from 1")),
                n => Ok(Some(n)),
            },
        }
    };
    let ignore = match args.get("ignorePrintAreas") {
        None | Some(Json::Null) => false,
        Some(v) => bool_field(verb, "ignorePrintAreas", v)?,
    };
    Ok(Job {
        what,
        ignore_print_areas: ignore,
        from: page("from")?,
        to: page("to")?,
    })
}

/// `print.pages`: the pages a print job prints on — each page's sheet, the
/// range of its body, the title rows and columns repeated on it, its page
/// number and scale — and the job's page count.
fn print_pages(app: &App, args: &Json) -> Result<Json, String> {
    let job = job_arg(app, "print.pages", args)?;
    let wb = &app.pkg.workbook;
    let pages = gridcore::print::paginate::paginate(wb, &job);
    // A job cut short is the error `wb.export-pdf` gives, not a short list.
    if pages.truncated {
        return Err(gridcore::print::pdf::PrintError::TooManyPages.to_string());
    }
    let span = |v: &[u32], rows: bool| match (v.first(), v.last()) {
        (Some(&a), Some(&b)) if rows => Json::Str(format!("{}:{}", a + 1, b + 1)),
        (Some(&a), Some(&b)) => Json::Str(format!("{}:{}", col_name(a), col_name(b))),
        _ => Json::Null,
    };
    let list = pages
        .pages
        .iter()
        .map(|p| {
            Json::obj(vec![
                ("sheet", Json::Num(p.sheet as f64)),
                ("name", Json::Str(wb.sheets[p.sheet].name.clone())),
                ("range", Json::Str(area::rect_name(p.range()))),
                ("number", Json::Num(p.number as f64)),
                ("titleRows", span(&p.title_rows, true)),
                ("titleCols", span(&p.title_cols, false)),
                ("scale", Json::Num((p.scale * 100.0).round())),
            ])
        })
        .collect();
    Ok(Json::obj(vec![
        ("total", Json::Num(pages.total as f64)),
        ("pages", Json::Arr(list)),
    ]))
}

/// `wb.export-pdf`: print a job (as `print.pages` takes it; the active sheet
/// by default) to a PDF at `path` (absolutized against this process's cwd),
/// refusing to overwrite an existing file. Nothing to print is an error and
/// writes no file. Neither marks the workbook modified nor touches undo.
fn wb_export_pdf(app: &App, args: &Json) -> Result<Json, String> {
    let path = args.get_str("path").ok_or("wb.export-pdf needs a 'path'")?;
    let job = job_arg(app, "wb.export-pdf", args)?;
    let abs =
        std::path::absolute(std::path::Path::new(path)).map_err(|e| format!("bad path: {e}"))?;
    if abs.exists() {
        return Err(format!("already exists: {}", abs.display()));
    }
    let (pdf, pages) =
        crate::print_pdf(&app.pkg.workbook, &job, &app.path).map_err(|e| e.to_string())?;
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create failed: {e}"))?;
    }
    opccore::fsio::create_atomic(&abs, &pdf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("already exists: {}", abs.display())
        } else {
            format!("create failed: {e}")
        }
    })?;
    Ok(Json::obj(vec![
        ("path", Json::Str(abs.display().to_string())),
        ("pages", Json::Num(pages as f64)),
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
    use gridcore::xlsx::{load_xlsx, new_xlsx, save_xlsx};

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

    fn get(app: &mut App, r: &str) -> Json {
        dispatch(
            app,
            "cell.get",
            &Json::obj(vec![("ref", Json::Str(r.into()))]),
        )
        .unwrap()
    }

    #[test]
    fn cell_set_apostrophe_is_a_quote_prefix_not_text_599() {
        let mut a = app();
        set(&mut a, "A1", "'007");
        let g = get(&mut a, "A1");
        assert_eq!(g.get_str("value"), Some("007"));
        assert_eq!(g.get_str("text"), Some("007"));
        let cell = a.pkg.workbook.sheets[0].cell(0, 0).unwrap().clone();
        assert!(a.pkg.workbook.styles.xf(cell.style).quote_prefix);
        // Saved: the shared string has no apostrophe, the xf says quotePrefix.
        let re = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&a.pkg)).unwrap();
        let styles = String::from_utf8(re.part("xl/styles.xml").unwrap().to_vec()).unwrap();
        assert!(styles.contains("quotePrefix=\"1\""), "{styles}");
        let shared = re
            .part("xl/sharedStrings.xml")
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .unwrap_or_default();
        assert!(!shared.contains("'007"), "{shared}");
        // The editor starts from the apostrophe again.
        a.cur = (0, 0);
        assert_eq!(a.current_input_text(), "'007");
    }

    #[test]
    fn cell_set_refuses_more_than_32767_characters_658() {
        let mut a = app();
        set(&mut a, "A1", "old");
        let long = "y".repeat(32_768);
        let err = cell_set(
            &mut a,
            &Json::obj(vec![
                ("ref", Json::Str("A1".into())),
                ("text", Json::Str(long)),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("32767"), "{err}");
        assert_eq!(get(&mut a, "A1").get_str("value"), Some("old"));
        set(&mut a, "A2", &"y".repeat(32_767));
        let err = dispatch(
            &mut a,
            "range.set",
            &Json::obj(vec![
                ("start", Json::Str("B1".into())),
                (
                    "rows",
                    Json::Arr(vec![Json::Arr(vec![
                        Json::Str("fine".into()),
                        Json::Str("y".repeat(32_768)),
                    ])]),
                ),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("C1"), "{err}");
        assert!(
            a.pkg.workbook.sheets[0].cell(0, 1).is_none(),
            "nothing applied"
        );
    }

    #[test]
    fn a_text_cell_takes_a_broken_formula_as_text_654() {
        let mut a = app();
        use gridcore::sheet::{NumFmt, Xf};
        let text = a.pkg.workbook.styles.intern(Xf {
            numfmt: NumFmt::Text,
            code: Some("@".into()),
            ..Xf::default()
        });
        for c in 0..2 {
            a.pkg.workbook.sheets[0].set_cell(
                0,
                c,
                Cell {
                    style: text,
                    ..Cell::default()
                },
            );
        }
        set(&mut a, "A1", "=SUM(");
        assert_eq!(get(&mut a, "A1").get_str("value"), Some("=SUM("));
        dispatch(
            &mut a,
            "range.set",
            &Json::obj(vec![
                ("start", Json::Str("B1".into())),
                (
                    "rows",
                    Json::Arr(vec![Json::Arr(vec![Json::Str("=1+".into())])]),
                ),
            ]),
        )
        .unwrap();
        assert_eq!(get(&mut a, "B1").get_str("value"), Some("=1+"));
        // A General cell still refuses it.
        assert!(
            cell_set(
                &mut a,
                &Json::obj(vec![
                    ("ref", Json::Str("C1".into())),
                    ("text", Json::Str("=SUM(".into()))
                ]),
            )
            .is_err()
        );
        // And the TUI's commit path.
        a.cur = (0, 0);
        a.edit = Some(crate::EditState {
            text: "=1+".into(),
            cursor: 3,
            replace: false,
            seed: None,
            proposal: None,
        });
        assert!(a.commit_edit());
        assert_eq!(get(&mut a, "A1").get_str("value"), Some("=1+"));
    }

    #[test]
    fn cell_set_recognises_a_date_and_reports_its_format_653() {
        let mut a = app();
        set(&mut a, "A1", "1/15/2024");
        let g = get(&mut a, "A1");
        assert_eq!(g.get("value").and_then(Json::as_f64), Some(45306.0));
        let fmt = g.get("format").expect("a format");
        assert_eq!(fmt.get_str("numFmt"), Some("m/d/yyyy"));
        set(&mut a, "A2", "1,234");
        assert_eq!(
            get(&mut a, "A2").get("value").and_then(Json::as_f64),
            Some(1234.0)
        );
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
    fn wb_path_lists_circular_references() {
        // #660: the circle's cells, active sheet first; none once broken.
        let mut a = app();
        let circular = |a: &mut App| {
            let r = dispatch(a, "wb.path", &Json::Null).unwrap();
            match r.get("circular") {
                Some(Json::Arr(v)) => v
                    .iter()
                    .map(|j| j.as_str().unwrap_or_default().to_string())
                    .collect::<Vec<_>>(),
                other => panic!("circular: {other:?}"),
            }
        };
        assert!(circular(&mut a).is_empty());
        set(&mut a, "E1", "=E1+1");
        set(&mut a, "F1", "=G1+1");
        set(&mut a, "G1", "=F1*2");
        assert_eq!(circular(&mut a), vec!["E1", "F1", "G1"]);
        let e1 = dispatch(
            &mut a,
            "cell.get",
            &Json::obj(vec![("ref", Json::Str("F1".into()))]),
        )
        .unwrap();
        assert_eq!(e1.get("text").and_then(|t| t.as_str()), Some("0"));
        set(&mut a, "E1", "1");
        set(&mut a, "G1", "2");
        assert!(circular(&mut a).is_empty());
    }

    #[test]
    fn wb_path_lists_an_opened_workbooks_circles() {
        // #660: a saved circle is listed straight after opening.
        use gridcore::sheet::Cell;
        use gridcore::xlsx::{load_xlsx, save_xlsx};
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 4, Cell::formula("E1+1"));
        let mut a = App::new(load_xlsx(&save_xlsx(&pkg)).unwrap(), "c.xlsx");
        a.os_clip = None;
        let r = dispatch(&mut a, "wb.path", &Json::Null).unwrap();
        match r.get("circular") {
            Some(Json::Arr(v)) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].as_str(), Some("E1"));
            }
            other => panic!("circular: {other:?}"),
        }
    }

    #[test]
    fn spill_and_calc_constants_are_refused_as_formulas() {
        // #657: Excel refuses these as formulas (as it does #FIELD!), but a
        // plain typed #SPILL! is the error value and #GETTING_DATA is a
        // formula constant.
        let mut a = app();
        for text in [
            "=#SPILL!",
            "=#CALC!",
            "=ERROR.TYPE(#SPILL!)",
            "=ERROR.TYPE(#CALC!)",
            "=#FIELD!",
        ] {
            let err = cell_set(
                &mut a,
                &Json::obj(vec![
                    ("ref", Json::Str("A1".into())),
                    ("text", Json::Str(text.into())),
                ]),
            )
            .unwrap_err();
            assert!(err.contains("formula error"), "{text}: {err}");
        }
        assert!(!a.modified);
        set(&mut a, "A2", "#SPILL!");
        assert_eq!(
            a.sheet().cell(1, 0).unwrap().value,
            CellValue::Error("#SPILL!".into())
        );
        let r = dispatch(
            &mut a,
            "formula.eval",
            &Json::obj(vec![(
                "formula",
                Json::Str("=ERROR.TYPE(#GETTING_DATA)".into()),
            )]),
        )
        .unwrap();
        assert_eq!(r.get("text").and_then(|t| t.as_str()), Some("8"));
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
        // Excel's CSV text: CR LF records (a file adds the BOM).
        assert_eq!(r.get_str("csv"), Some("name,amount\r\nAlice,30\r\n"));
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
            anchor_ix: 0,
            from: (0, 0),
            to: (10, 5),
            kind: DrawingKind::Chart(ChartData {
                title: "Sales".into(),
                kind: "bar".into(),
                categories: vec!["North".into(), "South".into()],
                series: vec![ChartSeries {
                    name: "Q1".into(),
                    values: vec![10.0, 20.0],
                    ..Default::default()
                }],
                ..Default::default()
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

    /// #605: the verb converts fields exactly as opening a `.csv` does.
    #[test]
    fn sheet_import_csv_converts_fields_as_typed() {
        let mut a = app();
        let r = dispatch(
            &mut a,
            "sheet.import-csv",
            &Json::obj(vec![(
                "text",
                Json::Str("sep=;\npct;calc\n12%;=1+1\n".into()),
            )]),
        )
        .unwrap();
        let idx = r.get_usize("sheet").unwrap();
        let sh = &a.pkg.workbook.sheets[idx];
        assert_eq!(sh.cell(0, 0).unwrap().value, CellValue::Text("pct".into()));
        assert_eq!(sh.cell(1, 0).unwrap().value, CellValue::Number(0.12));
        assert_eq!(sh.cell(1, 1).unwrap().value, CellValue::Number(2.0));
        assert_eq!(r.get_usize("rows"), Some(2));
    }

    fn get_value(a: &App, r: &str) -> CellValue {
        let (row, col) = parse_cell_name(r).unwrap();
        a.pkg.workbook.sheets[a.sheet]
            .cell(row, col)
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }

    fn opts(pairs: Vec<(&str, Json)>) -> Json {
        Json::obj(pairs)
    }

    fn strs(v: &[&str]) -> Json {
        Json::Arr(v.iter().map(|s| Json::Str(s.to_string())).collect())
    }

    /// #607: the Text Import Wizard's example through the control surface.
    #[test]
    fn sheet_import_text_applies_the_wizard_options() {
        let mut a = app();
        let r = dispatch(
            &mut a,
            "sheet.import-text",
            &Json::obj(vec![
                (
                    "text",
                    Json::Str("02134\t03/04/2024\t1.234,5-\tx\r\n".into()),
                ),
                (
                    "options",
                    opts(vec![
                        ("columns", strs(&["text", "date:dmy", "general", "skip"])),
                        ("decimal", Json::Str(",".into())),
                        ("thousands", Json::Str(".".into())),
                        ("trailing_minus", Json::Bool(true)),
                    ]),
                ),
            ]),
        )
        .unwrap();
        assert_eq!(
            (r.get_usize("rows"), r.get_usize("cols")),
            (Some(1), Some(3))
        );
        let sh = &a.pkg.workbook.sheets[r.get_usize("sheet").unwrap()];
        assert_eq!(
            sh.cell(0, 0).unwrap().value,
            CellValue::Text("02134".into())
        );
        let apr3 = gridcore::sheet::parts_to_serial(2024, 4, 3, 0, false);
        assert_eq!(sh.cell(0, 1).unwrap().value, CellValue::Number(apr3));
        let b1 = a.pkg.workbook.styles.xf(sh.cell(0, 1).unwrap().style);
        assert_eq!(b1.numfmt, gridcore::sheet::NumFmt::Date);
        assert_eq!(sh.cell(0, 2).unwrap().value, CellValue::Number(-1234.5));
        assert!(sh.cell(0, 3).is_none());
    }

    #[test]
    fn sheet_import_text_reads_fixed_width_start_row_and_origin() {
        let dir = std::env::temp_dir().join(format!("xlsxy-import-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixed.prn");
        std::fs::write(&path, b"skip me\r\nZ\xFCr  12\r\n").unwrap();
        let mut a = app();
        let r = dispatch(
            &mut a,
            "sheet.import-text",
            &Json::obj(vec![
                ("path", Json::Str(path.to_str().unwrap().into())),
                (
                    "options",
                    opts(vec![
                        ("kind", Json::Str("fixed".into())),
                        ("breaks", Json::Arr(vec![Json::Num(5.0)])),
                        ("start_row", Json::Num(2.0)),
                        ("origin", Json::Str("windows-1252".into())),
                    ]),
                ),
            ]),
        )
        .unwrap();
        let sh = &a.pkg.workbook.sheets[r.get_usize("sheet").unwrap()];
        assert_eq!(
            sh.cell(0, 0).unwrap().value,
            CellValue::Text("Z\u{fc}r".into())
        );
        assert_eq!(sh.cell(0, 1).unwrap().value, CellValue::Number(12.0));
        let bad = dispatch(
            &mut a,
            "sheet.import-text",
            &Json::obj(vec![
                ("text", Json::Str("a".into())),
                ("options", opts(vec![("columns", strs(&["bogus"]))])),
            ]),
        );
        assert!(bad.unwrap_err().contains("unknown column format"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #692: the verb converts one column, refusing to overwrite data unless
    /// told to, as one undo step.
    #[test]
    fn range_text_to_columns_asks_before_replacing() {
        let mut a = app();
        set(&mut a, "A1", "Ink,,Red,7");
        set(&mut a, "D1", "keep");
        let args = |replace: Option<bool>| {
            let mut v = vec![
                ("range", Json::Str("A1".into())),
                ("options", opts(vec![("delimiters", strs(&["comma"]))])),
            ];
            if let Some(b) = replace {
                v.push(("replace", Json::Bool(b)));
            }
            Json::obj(v)
        };
        let err = dispatch(&mut a, "range.text-to-columns", &args(None)).unwrap_err();
        assert!(err.contains(gridcore::edit::TTC_REPLACE), "{err}");
        assert_eq!(get_value(&a, "A1"), CellValue::Text("Ink,,Red,7".into()));
        let r = dispatch(&mut a, "range.text-to-columns", &args(Some(true))).unwrap();
        assert_eq!(r.get_usize("rows"), Some(1));
        assert_eq!(get_value(&a, "A1"), CellValue::Text("Ink".into()));
        assert_eq!(get_value(&a, "B1"), CellValue::Empty);
        assert_eq!(get_value(&a, "C1"), CellValue::Text("Red".into()));
        assert_eq!(get_value(&a, "D1"), CellValue::Number(7.0));
        a.undo();
        assert_eq!(get_value(&a, "D1"), CellValue::Text("keep".into()));
        let two = Json::obj(vec![("range", Json::Str("A1:B2".into()))]);
        let err = dispatch(&mut a, "range.text-to-columns", &two).unwrap_err();
        assert_eq!(err, gridcore::edit::TTC_ONE_COLUMN);
    }

    /// #607: the options are readable and settable, and the next CSV import
    /// honours them.
    #[test]
    fn app_options_switch_the_automatic_data_conversion() {
        let mut a = app();
        let all = dispatch(&mut a, "app.options", &Json::Null).unwrap();
        assert_eq!(all.get("convert_dates"), Some(&Json::Bool(true)));
        let off = Json::obj(vec![
            ("convert_leading_zeros", Json::Bool(false)),
            ("convert_long_numbers", Json::Bool(false)),
            ("convert_e_notation", Json::Bool(false)),
            ("convert_dates", Json::Bool(false)),
        ]);
        let r = dispatch(&mut a, "app.options", &off).unwrap();
        assert_eq!(r.get("convert_leading_zeros"), Some(&Json::Bool(false)));
        let r = dispatch(
            &mut a,
            "sheet.import-csv",
            &Json::obj(vec![(
                "text",
                Json::Str("007,1/2,1E5,1234567890123456789\n".into()),
            )]),
        )
        .unwrap();
        let sh = &a.pkg.workbook.sheets[r.get_usize("sheet").unwrap()];
        for (c, t) in ["007", "1/2", "1E5", "1234567890123456789"]
            .iter()
            .enumerate()
        {
            assert_eq!(
                sh.cell(0, c as u32).unwrap().value,
                CellValue::Text(t.to_string())
            );
        }
        let bad = Json::obj(vec![
            ("convert_leading_zeros", Json::Bool(true)),
            ("convert_dates", Json::Str("no".into())),
        ]);
        assert!(dispatch(&mut a, "app.options", &bad).is_err());
        // The bad key refused the whole call: the good one was not applied.
        assert!(!a.auto_convert.remove_leading_zeros);
    }

    /// #672: the Editing options read and set through `app.options`, all or
    /// nothing like the conversion switches.
    #[test]
    fn app_options_set_the_editing_options() {
        let mut a = app();
        let all = dispatch(&mut a, "app.options", &Json::Null).unwrap();
        assert_eq!(all.get("edit_fixed_decimal"), Some(&Json::Bool(false)));
        assert_eq!(all.get("edit_fixed_decimal_places"), Some(&Json::Num(2.0)));
        assert_eq!(
            all.get("edit_move_direction"),
            Some(&Json::Str("down".into()))
        );
        let set = Json::obj(vec![
            ("edit_fixed_decimal", Json::Bool(true)),
            ("edit_fixed_decimal_places", Json::Num(-2.0)),
            ("edit_move_direction", Json::Str("Right".into())),
            ("edit_autocomplete", Json::Bool(false)),
        ]);
        let r = dispatch(&mut a, "app.options", &set).unwrap();
        assert_eq!(
            r.get("edit_move_direction"),
            Some(&Json::Str("right".into()))
        );
        assert!(a.edit_opts.fixed_decimal && !a.edit_opts.autocomplete);
        assert_eq!(a.edit_opts.places, -2);
        assert_eq!(a.edit_opts.enter_move, gridcore::options::EnterMove::Right);
        for bad in [
            ("edit_fixed_decimal_places", Json::Num(301.0)),
            ("edit_fixed_decimal_places", Json::Num(1.5)),
            ("edit_move_direction", Json::Str("sideways".into())),
            ("edit_in_cell", Json::Str("no".into())),
        ] {
            let args = Json::obj(vec![("edit_fixed_decimal", Json::Bool(false)), bad]);
            assert!(dispatch(&mut a, "app.options", &args).is_err());
            assert!(a.edit_opts.fixed_decimal, "a refused call sets nothing");
        }
    }

    /// #672: automation never shifts a number, whatever the user's fixed
    /// decimal point: only typing into the grid does.
    #[test]
    fn cell_set_ignores_the_fixed_decimal() {
        let mut a = app();
        a.edit_opts.fixed_decimal = true;
        set(&mut a, "A1", "1234");
        assert_eq!(
            a.pkg.workbook.sheets[0].cell(0, 0).unwrap().value,
            CellValue::Number(1234.0)
        );
    }

    /// A workbook saved as Text (Tab delimited) is bound to its .txt, so
    /// wb.reload re-imports that text rather than leaving a wizard open.
    #[test]
    fn wb_reload_of_a_text_file_reimports_it_without_the_wizard() {
        let dir = std::env::temp_dir().join(format!("xlsxy-reload-txt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("saved.txt");
        let mut a = app();
        set(&mut a, "A1", "a");
        set(&mut a, "B1", "1");
        a.request_save_as(path.to_string_lossy().into_owned());
        assert_eq!(std::path::Path::new(&a.path), path);
        std::fs::write(&path, "b\t2\r\n").unwrap();
        dispatch(&mut a, "wb.reload", &Json::Null).unwrap();
        assert!(a.text_dialog.is_none());
        // Still bound to the text file and its type: a save writes it, not a
        // saved.xlsx beside it.
        assert_eq!(std::path::Path::new(&a.path), path);
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        assert!(!dir.join("saved.xlsx").exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"b\t2\r\n");
        assert_eq!(get_value(&a, "A1"), CellValue::Text("b".into()));
        assert_eq!(get_value(&a, "B1"), CellValue::Number(2.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Formatted Text and Web Page cannot be read back: reload refuses and
    /// changes nothing.
    #[test]
    fn wb_reload_refuses_a_workbook_bound_to_prn_or_a_web_page() {
        let dir = std::env::temp_dir().join(format!("xlsxy-reload-prn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (file, label) in [
            ("saved.prn", "Formatted Text (Space delimited)"),
            ("page.htm", "Web Page"),
        ] {
            let path = dir.join(file);
            let mut a = app();
            set(&mut a, "A1", "a");
            set(&mut a, "B1", "1");
            a.request_save_as(path.to_string_lossy().into_owned());
            let before = std::fs::read(&path).unwrap();
            let err = dispatch(&mut a, "wb.reload", &Json::Null).unwrap_err();
            assert_eq!(
                err,
                format!("{label} cannot be read back; reload is not available for this file")
            );
            assert_eq!(std::path::Path::new(&a.path), path);
            assert_eq!(get_value(&a, "B1"), CellValue::Number(1.0));
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A CSV-bound workbook reloads with the delimiter it was saved with:
    /// a first row with as many `;` as `,` is not re-split, and saving again
    /// writes the same bytes.
    #[test]
    fn wb_reload_of_a_csv_keeps_its_delimiter() {
        let dir = std::env::temp_dir().join(format!("xlsxy-reload-csv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("book.csv");
        let mut a = app();
        set(&mut a, "A1", "Name;Alias");
        set(&mut a, "B1", "x");
        set(&mut a, "A2", "'1,5");
        a.request_save_as(path.to_string_lossy().into_owned());
        let first = std::fs::read(&path).unwrap();
        dispatch(&mut a, "wb.reload", &Json::Null).unwrap();
        assert_eq!(get_value(&a, "A1"), CellValue::Text("Name;Alias".into()));
        assert_eq!(get_value(&a, "B1"), CellValue::Text("x".into()));
        assert_eq!(get_value(&a, "A2"), CellValue::Text("1,5".into()));
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A workbook bound to CSV (Comma delimited) reloads in Windows-1252:
    /// an é (C3 A9 as 1252 bytes: Ã©) comes back as it was, byte for byte.
    #[test]
    fn wb_reload_keeps_the_bound_encoding() {
        let dir = std::env::temp_dir().join(format!("xlsxy-reload-1252-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("latin.csv");
        let mut a = app();
        set(&mut a, "A1", "Ã©");
        let t = super::super::SAVE_TYPES
            .iter()
            .position(|t| t.label == "CSV (Comma delimited)")
            .unwrap();
        a.save_as_type(path.to_string_lossy().into_owned(), t);
        let first = std::fs::read(&path).unwrap();
        assert_eq!(first, b"\xC3\xA9\r\n");
        dispatch(&mut a, "wb.reload", &Json::Null).unwrap();
        assert_eq!(get_value(&a, "A1"), CellValue::Text("Ã©".into()));
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Text to Columns on another sheet leaves the current sheet as it is,
    /// and its undo and redo restore that other sheet.
    #[test]
    fn range_text_to_columns_on_another_sheet_keeps_the_current_one() {
        let mut a = app();
        dispatch(&mut a, "sheet.add", &Json::Null).unwrap();
        a.sheet = 0;
        let s1 = |a: &App, r: u32, c: u32| {
            a.pkg.workbook.sheets[1]
                .cell(r, c)
                .map(|c| c.value.clone())
                .unwrap_or_default()
        };
        a.pkg.workbook.sheets[1].set_cell(0, 0, gridcore::sheet::Cell::text("a\tb"));
        a.rebuild_engine();
        dispatch(
            &mut a,
            "range.text-to-columns",
            &Json::obj(vec![
                ("range", Json::Str("A1".into())),
                ("sheet", Json::Num(1.0)),
            ]),
        )
        .unwrap();
        assert_eq!(a.sheet, 0);
        assert_eq!(s1(&a, 0, 1), CellValue::Text("b".into()));
        a.undo();
        assert_eq!(s1(&a, 0, 0), CellValue::Text("a\tb".into()));
        assert_eq!(s1(&a, 0, 1), CellValue::Empty);
        a.redo();
        assert_eq!(s1(&a, 0, 1), CellValue::Text("b".into()));
    }

    /// A failed load is the verb's error, not a quiet success.
    #[test]
    fn wb_open_of_a_missing_workbook_is_an_error() {
        let mut a = app();
        let missing = std::env::temp_dir()
            .join("xlsxy-no-such-dir-704")
            .join("x.xlsx");
        let err = dispatch(
            &mut a,
            "wb.open",
            &Json::obj(vec![("path", Json::Str(missing.to_string_lossy().into()))]),
        );
        assert!(err.is_err());
    }

    /// An import into a 1904-system workbook reads dates in that system.
    #[test]
    fn sheet_import_csv_follows_the_workbooks_date_system() {
        let mut a = app();
        a.pkg.workbook.date1904 = true;
        let r = dispatch(
            &mut a,
            "sheet.import-csv",
            &Json::obj(vec![("text", Json::Str("1/2/2024\n".into()))]),
        )
        .unwrap();
        let wb = &a.pkg.workbook;
        let cell = wb.sheets[r.get_usize("sheet").unwrap()].cell(0, 0).unwrap();
        let shown = gridcore::sheet::format_with(&wb.styles.xf(cell.style), &cell.value, true);
        assert_eq!(shown, "1/2/2024");
    }

    #[test]
    fn wb_open_of_a_text_file_imports_it_with_the_wizard_defaults() {
        let dir = std::env::temp_dir().join(format!("xlsxy-open-txt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tabs.txt");
        std::fs::write(&path, "a\t1\n").unwrap();
        let mut a = app();
        dispatch(
            &mut a,
            "wb.open",
            &Json::obj(vec![("path", Json::Str(path.to_str().unwrap().into()))]),
        )
        .unwrap();
        assert!(a.text_dialog.is_none());
        assert_eq!(get_value(&a, "B1"), CellValue::Number(1.0));
        assert_eq!(std::path::Path::new(&a.path), path.with_extension("xlsx"));
        let _ = std::fs::remove_dir_all(&dir);
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
    fn sheet_remove_of_a_lower_sheet_makes_a_pending_cut_a_copy() {
        // #782: removing a sheet below the cut's source renumbers it; the
        // pending cut must not then clear the sheet that took its index.
        let mut a = app();
        for name in ["Second", "Third"] {
            dispatch(
                &mut a,
                "sheet.add",
                &Json::obj(vec![("name", Json::Str(name.into()))]),
            )
            .unwrap();
        }
        let set_on = |a: &mut App, s: usize, text: &str| {
            a.apply_on(s, vec![(0, 0, gridcore::edit::parse_input(text))]);
        };
        set_on(&mut a, 1, "2");
        set_on(&mut a, 2, "3");
        a.goto_sheet(1);
        a.cur = (0, 0);
        a.copy(true);
        dispatch(
            &mut a,
            "sheet.remove",
            &Json::obj(vec![("sheet", Json::Num(0.0))]),
        )
        .unwrap();
        // Second is now sheet 0 and Third sheet 1: the cut's recorded index
        // is in range but names Third.
        a.goto_sheet(0);
        a.cur = (0, 5);
        a.paste();
        let text =
            |a: &App, s: usize, r, c| a.pkg.workbook.sheets[s].cell(r, c).map(|x| x.value.clone());
        assert_eq!(text(&a, 0, 0, 0), Some(CellValue::Number(2.0)));
        assert_eq!(text(&a, 1, 0, 0), Some(CellValue::Number(3.0)));
        assert_eq!(text(&a, 0, 0, 5), Some(CellValue::Number(2.0)));
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

    /// A one-sheet `Report` workbook with a print area and a manual row break
    /// before 0-based row 13.
    fn print_setup_book() -> Vec<u8> {
        let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let pkg_rel = "http://schemas.openxmlformats.org/package/2006/relationships";
        let parts: Vec<(String, Vec<u8>)> = vec![
            (
                "[Content_Types].xml".into(),
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#.into(),
            ),
            (
                "_rels/.rels".into(),
                format!(r#"<?xml version="1.0"?><Relationships xmlns="{pkg_rel}"><Relationship Id="rId1" Type="{rel}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#).into_bytes(),
            ),
            (
                "xl/workbook.xml".into(),
                format!(r#"<?xml version="1.0"?><workbook xmlns="{ns}" xmlns:r="{rel}"><sheets><sheet name="Report" sheetId="1" r:id="rId1"/></sheets><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">Report!$A$1:$D$20</definedName></definedNames></workbook>"#).into_bytes(),
            ),
            (
                "xl/_rels/workbook.xml.rels".into(),
                format!(r#"<?xml version="1.0"?><Relationships xmlns="{pkg_rel}"><Relationship Id="rId1" Type="{rel}/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#).into_bytes(),
            ),
            (
                "xl/worksheets/sheet1.xml".into(),
                format!(r#"<?xml version="1.0"?><worksheet xmlns="{ns}"><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><rowBreaks count="1" manualBreakCount="1"><brk id="13" max="16383" man="1"/></rowBreaks></worksheet>"#).into_bytes(),
            ),
        ];
        opccore::zipwrite::write_zip(&parts)
    }

    /// The saved workbook.xml and sheet1.xml of what `a` would write now.
    fn saved_print_setup(a: &mut App) -> (String, String) {
        let pkg = gridcore::xlsx::load_xlsx(&a.package_bytes()).expect("saved file reloads");
        let part = |n: &str| String::from_utf8_lossy(pkg.part(n).unwrap()).into_owned();
        (part("xl/workbook.xml"), part("xl/worksheets/sheet1.xml"))
    }

    #[test]
    fn row_insert_moves_print_area_and_breaks_and_undo_puts_them_back() {
        let pkg = gridcore::xlsx::load_xlsx(&print_setup_book()).unwrap();
        let mut a = App::new(pkg, "ctl-print-setup.xlsx");
        a.os_clip = None;
        dispatch(
            &mut a,
            "row.insert",
            &Json::obj(vec![("at", Json::Num(0.0))]),
        )
        .unwrap();
        let (wb, ws) = saved_print_setup(&mut a);
        assert!(wb.contains(">Report!$A$2:$D$21</definedName>"), "{wb}");
        assert!(ws.contains(r#"<brk id="14" max="16383" man="1"/>"#), "{ws}");

        a.undo();
        let (wb, ws) = saved_print_setup(&mut a);
        assert!(wb.contains(">Report!$A$1:$D$20</definedName>"), "{wb}");
        assert!(
            ws.contains(r#"<rowBreaks count="1" manualBreakCount="1"><brk id="13" max="16383" man="1"/></rowBreaks>"#),
            "{ws}"
        );
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
    fn cell_format_over_a_spill_keeps_it_as_one_undo_group() {
        // #784: restyling a spilled block changes styles only.
        let mut a = app();
        set(&mut a, "D1", "=SEQUENCE(3)");
        let spill = |a: &App| a.pkg.workbook.sheets[0].cell(0, 3).unwrap().spill;
        assert_eq!(spill(&a), Some((3, 1)));
        let depth = a.undo.len();
        dispatch(
            &mut a,
            "cell.format",
            &Json::obj(vec![
                ("range", Json::Str("D1:D3".into())),
                ("patch", Json::obj(vec![("bold", Json::Bool(true))])),
            ]),
        )
        .unwrap();
        assert_eq!(a.undo.len(), depth + 1);
        assert_eq!(spill(&a), Some((3, 1)));
        for r in 0..3u32 {
            let cell = a.pkg.workbook.sheets[0].cell(r, 3).unwrap();
            assert_eq!(cell.value, CellValue::Number(f64::from(r + 1)));
            assert!(a.pkg.workbook.styles.xf(cell.style).bold);
        }
        a.undo();
        assert_eq!(spill(&a), Some((3, 1)));
        assert_eq!(a.pkg.workbook.sheets[0].cell(0, 3).unwrap().style, 0);
        a.redo();
        assert_eq!(spill(&a), Some((3, 1)));
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

    #[test]
    fn wb_save_reports_failure_and_keeps_modified() {
        let dir = std::env::temp_dir().join(format!("xlsxy-609-save-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // The parent directory does not exist, so the write fails.
        let book = dir.join("missing").join("book.xlsx");
        let mut a = App::new(new_xlsx(), book.to_str().unwrap());
        a.os_clip = None;
        set(&mut a, "A1", "changed");

        let err = dispatch(&mut a, "wb.save", &Json::Null).unwrap_err();
        assert!(err.starts_with("save failed: "), "{err}");
        // The error is the status bar's text, word for word.
        assert_eq!(a.status.as_deref(), Some(err.as_str()));
        assert!(a.modified, "a failed save must leave the workbook modified");
        assert_eq!(std::path::Path::new(&a.path), book);
        assert!(!book.exists());

        // Fixing the cause lets the next wb.save succeed.
        std::fs::create_dir_all(book.parent().unwrap()).unwrap();
        let r = dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        assert_eq!(r.get("modified").unwrap().as_bool(), Some(false));
        assert!(book.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The corpus workbook with its `backupFile` attribute rewritten; the
    /// bytes are a loadable .xlsx with the flag on or off.
    fn backup_fixture(flag: &str) -> Vec<u8> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../corpus/xlsx/calc-3d.xlsx");
        let bytes = std::fs::read(path).expect("corpus/xlsx/calc-3d.xlsx exists");
        let mut pkg = load_xlsx(&bytes).expect("corpus loads");
        let wb = pkg
            .part("xl/workbook.xml")
            .expect("workbook part is xl/workbook.xml");
        let xml = String::from_utf8_lossy(wb)
            .replace("backupFile=\"false\"", &format!("backupFile=\"{flag}\""));
        pkg.set_part("xl/workbook.xml", xml.into_bytes());
        save_xlsx(&pkg)
    }

    /// Excel's *Always create backup*: wb.save over an existing file keeps
    /// the previous bytes as `Backup of <stem>.xlk` beside it, and each
    /// later save refreshes the backup from the file it replaces.
    #[test]
    fn wb_save_keeps_backup_of_previous_file() {
        let dir =
            std::env::temp_dir().join(format!("xlsxy-608-backup-keeps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("book.xlsx");
        std::fs::write(&book, backup_fixture("1")).unwrap();
        let before = std::fs::read(&book).unwrap();
        let mut a = App::new(load_xlsx(&before).unwrap(), book.to_str().unwrap());
        a.os_clip = None;
        set(&mut a, "A1", "changed");
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();

        let backup = dir.join("Backup of book.xlk");
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            before,
            "backup holds the pre-save bytes"
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "book.xlsx and its backup only"
        );
        let re = load_xlsx(&std::fs::read(&book).unwrap()).unwrap();
        assert_eq!(
            re.workbook.sheets[0].cell(0, 0).unwrap().value,
            CellValue::Text("changed".into())
        );

        // The next save replaces the backup with the first save's file.
        let after1 = std::fs::read(&book).unwrap();
        set(&mut a, "A1", "again");
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), after1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Without the flag nothing extra appears in the folder.
    #[test]
    fn wb_save_without_backup_flag_keeps_no_backup() {
        let dir = std::env::temp_dir().join(format!("xlsxy-608-backup-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("book.xlsx");
        std::fs::write(&book, backup_fixture("false")).unwrap();
        let before = std::fs::read(&book).unwrap();
        let mut a = App::new(load_xlsx(&before).unwrap(), book.to_str().unwrap());
        a.os_clip = None;
        set(&mut a, "A1", "changed");
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "book.xlsx only"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A first save to a not-yet-existing path has no previous version to
    /// keep: the save succeeds and makes no backup.
    #[test]
    fn wb_save_first_save_with_backup_flag_makes_no_backup() {
        let dir =
            std::env::temp_dir().join(format!("xlsxy-608-backup-first-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("new.xlsx");
        let mut a = App::new(
            load_xlsx(&backup_fixture("1")).unwrap(),
            book.to_str().unwrap(),
        );
        a.os_clip = None;
        set(&mut a, "A1", "changed");
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        assert!(book.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "new.xlsx only");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A backup that cannot be written aborts the save before the workbook
    /// file is touched, and the workbook stays modified.
    #[test]
    fn wb_save_fails_when_backup_cannot_be_written() {
        let dir =
            std::env::temp_dir().join(format!("xlsxy-608-backup-fails-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("book.xlsx");
        std::fs::write(&book, backup_fixture("1")).unwrap();
        let before = std::fs::read(&book).unwrap();
        // A directory in the backup's name blocks the write.
        std::fs::create_dir(dir.join("Backup of book.xlk")).unwrap();
        let mut a = App::new(load_xlsx(&before).unwrap(), book.to_str().unwrap());
        a.os_clip = None;
        set(&mut a, "A1", "changed");
        let err = dispatch(&mut a, "wb.save", &Json::Null).unwrap_err();
        assert!(err.starts_with("save failed: "), "{err}");
        assert!(err.contains("Backup of book.xlk"), "{err}");
        assert_eq!(a.status.as_deref(), Some(err.as_str()));
        assert!(a.modified, "a failed save must leave the workbook modified");
        assert_eq!(std::fs::read(&book).unwrap(), before, "book.xlsx untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A backup of a private workbook must not become world-readable:
    /// keep_backup mirrors the file's own permissions.
    #[cfg(unix)]
    #[test]
    fn wb_save_backup_keeps_source_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("xlsxy-608-backup-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("book.xlsx");
        std::fs::write(&book, backup_fixture("1")).unwrap();
        std::fs::set_permissions(&book, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut a = App::new(
            load_xlsx(&std::fs::read(&book).unwrap()).unwrap(),
            book.to_str().unwrap(),
        );
        a.os_clip = None;
        set(&mut a, "A1", "changed");
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        let backup = dir.join("Backup of book.xlk");
        let mode = std::fs::metadata(&backup).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "backup mirrors the source's permissions"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A read-only book must not produce a read-only backup: the save of
    /// the read-only book fails, but once the book is writable again the
    /// next save must be able to replace the backup.
    #[cfg(unix)]
    #[test]
    fn wb_save_backup_of_readonly_book_stays_replaceable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("xlsxy-608-backup-ro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("book.xlsx");
        std::fs::write(&book, backup_fixture("1")).unwrap();
        std::fs::set_permissions(&book, std::fs::Permissions::from_mode(0o444)).unwrap();
        let mut a = App::new(
            load_xlsx(&std::fs::read(&book).unwrap()).unwrap(),
            book.to_str().unwrap(),
        );
        a.os_clip = None;
        set(&mut a, "A1", "changed");
        // The read-only book cannot be replaced, so the save fails — but
        // the backup is still kept, and it must be owner-writable.
        dispatch(&mut a, "wb.save", &Json::Null).unwrap_err();
        let backup = dir.join("Backup of book.xlk");
        let mode = std::fs::metadata(&backup).unwrap().permissions().mode();
        assert_ne!(mode & 0o200, 0, "backup stays owner-writable, got {mode:o}");
        // Once the book is writable, saving replaces the backup again.
        std::fs::set_permissions(&book, std::fs::Permissions::from_mode(0o644)).unwrap();
        let before_second = std::fs::read(&book).unwrap();
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), before_second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The issue's repro: another program holds the file open with no
    /// sharing, so the atomic replace fails.
    #[cfg(windows)]
    #[test]
    fn wb_save_reports_failure_while_file_is_locked() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir =
            std::env::temp_dir().join(format!("xlsxy-609-save-locked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("book.xlsx");
        let mut a = App::new(new_xlsx(), book.to_str().unwrap());
        a.os_clip = None;
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        let before = std::fs::read(&book).unwrap();

        set(&mut a, "A1", "changed");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&book)
            .unwrap();
        let err = dispatch(&mut a, "wb.save", &Json::Null).unwrap_err();
        assert!(err.starts_with("save failed: "), "{err}");
        assert!(a.modified);
        drop(lock);
        assert_eq!(std::fs::read(&book).unwrap(), before, "old file intact");

        let r = dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        assert_eq!(r.get("modified").unwrap().as_bool(), Some(false));
        assert_ne!(std::fs::read(&book).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A book locked by another process fails in keep_backup's read, so the
    /// error names the book, not the backup that was never touched.
    #[cfg(windows)]
    #[test]
    fn wb_save_with_backup_flag_reports_locked_book() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir =
            std::env::temp_dir().join(format!("xlsxy-608-save-locked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("book.xlsx");
        std::fs::write(&book, backup_fixture("1")).unwrap();
        let mut a = App::new(
            load_xlsx(&std::fs::read(&book).unwrap()).unwrap(),
            book.to_str().unwrap(),
        );
        a.os_clip = None;
        set(&mut a, "A1", "changed");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&book)
            .unwrap();
        let err = dispatch(&mut a, "wb.save", &Json::Null).unwrap_err();
        assert!(err.starts_with("save failed: cannot read"), "{err}");
        assert!(!err.contains("Backup of"), "{err}");
        assert!(!dir.join("Backup of book.xlk").exists());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- #600: document properties ------------------------------------------

    fn part_text(pkg: &gridcore::xlsx::SheetPackage, name: &str) -> String {
        String::from_utf8_lossy(pkg.part(name).unwrap_or_else(|| panic!("no part {name}")))
            .into_owned()
    }

    /// The text between `<open>` and `</close>` of the first `tag` element.
    fn element_text(xml: &str, tag: &str) -> String {
        let open = xml
            .find(&format!("<{tag}"))
            .unwrap_or_else(|| panic!("no <{tag}> in {xml}"));
        let start = open + xml[open..].find('>').unwrap() + 1;
        let end = start + xml[start..].find(&format!("</{tag}>")).unwrap();
        xml[start..end].to_string()
    }

    /// The issue's openpyxl `props.xlsx`: sheets Alpha and Beta, created by
    /// "Author One", last modified by "Author Two" on 2020-01-02, with an
    /// Excel-shaped app.xml that also lists a named range.
    fn props_fixture() -> Vec<u8> {
        let mut pkg = new_xlsx();
        pkg.rename_sheet(0, "Alpha");
        pkg.add_sheet("Beta");
        pkg.set_part(
            "docProps/core.xml",
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:creator>Author One</dc:creator><cp:lastModifiedBy>Author Two</cp:lastModifiedBy><dcterms:created xsi:type="dcterms:W3CDTF">2020-01-02T03:04:05Z</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">2020-01-02T03:04:05Z</dcterms:modified></cp:coreProperties>"#
                .to_vec(),
        );
        pkg.set_part(
            "docProps/app.xml",
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>Microsoft Excel</Application><HeadingPairs><vt:vector size="4" baseType="variant"><vt:variant><vt:lpstr>Worksheets</vt:lpstr></vt:variant><vt:variant><vt:i4>2</vt:i4></vt:variant><vt:variant><vt:lpstr>Named Ranges</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant></vt:vector></HeadingPairs><TitlesOfParts><vt:vector size="3" baseType="lpstr"><vt:lpstr>Alpha</vt:lpstr><vt:lpstr>Beta</vt:lpstr><vt:lpstr>Total</vt:lpstr></vt:vector></TitlesOfParts></Properties>"#
                .to_vec(),
        );
        let rels = part_text(&pkg, "_rels/.rels").replace(
            "</Relationships>",
            r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>"#,
        );
        pkg.set_part("_rels/.rels", rels.into_bytes());
        save_xlsx(&pkg)
    }

    /// The issue's repro, end to end: rename Beta → Gamma, set A1, save.
    #[test]
    fn wb_save_stamps_core_properties_and_refreshes_titles_of_parts() {
        let dir = std::env::temp_dir().join(format!("xlsxy-600-props-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("props.xlsx");
        std::fs::write(&book, props_fixture()).unwrap();
        let mut a = App::new(
            load_xlsx(&std::fs::read(&book).unwrap()).unwrap(),
            book.to_str().unwrap(),
        );
        a.os_clip = None;
        dispatch(
            &mut a,
            "sheet.rename",
            &Json::obj(vec![
                ("sheet", Json::Str("Beta".into())),
                ("name", Json::Str("Gamma".into())),
            ]),
        )
        .unwrap();
        set(&mut a, "A1", "1");
        let before = iso_now();
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        let after = iso_now();

        let saved = load_xlsx(&std::fs::read(&book).unwrap()).unwrap();
        let core = part_text(&saved, "docProps/core.xml");
        let modified = element_text(&core, "dcterms:modified");
        assert!(
            before <= modified && modified <= after,
            "{before} <= {modified} <= {after}"
        );
        assert_eq!(modified.len(), "2026-10-01T12:00:00Z".len());
        assert!(modified.ends_with('Z') && modified.as_bytes()[10] == b'T');
        assert!(core.contains(r#"<dcterms:modified xsi:type="dcterms:W3CDTF">"#));
        assert_eq!(element_text(&core, "cp:lastModifiedBy"), comment_author());
        assert_eq!(element_text(&core, "dc:creator"), "Author One");
        assert_eq!(
            element_text(&core, "dcterms:created"),
            "2020-01-02T03:04:05Z"
        );

        let app = part_text(&saved, "docProps/app.xml");
        assert!(app.contains(
            r#"<vt:variant><vt:lpstr>Worksheets</vt:lpstr></vt:variant><vt:variant><vt:i4>2</vt:i4></vt:variant><vt:variant><vt:lpstr>Named Ranges</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant>"#
        ), "{app}");
        assert!(app.contains(
            r#"<vt:vector size="3" baseType="lpstr"><vt:lpstr>Alpha</vt:lpstr><vt:lpstr>Gamma</vt:lpstr><vt:lpstr>Total</vt:lpstr></vt:vector>"#
        ), "{app}");

        // Removing a sheet shrinks the group; the named range stays.
        dispatch(
            &mut a,
            "sheet.remove",
            &Json::obj(vec![("sheet", Json::Str("Alpha".into()))]),
        )
        .unwrap();
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        let saved = load_xlsx(&std::fs::read(&book).unwrap()).unwrap();
        let app = part_text(&saved, "docProps/app.xml");
        assert!(app.contains(
            r#"<vt:vector size="4" baseType="variant"><vt:variant><vt:lpstr>Worksheets</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant>"#
        ), "{app}");
        assert!(app.contains(
            r#"<vt:vector size="2" baseType="lpstr"><vt:lpstr>Gamma</vt:lpstr><vt:lpstr>Total</vt:lpstr></vt:vector>"#
        ), "{app}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A workbook with no core properties gets them on its first save, with
    /// the package relationship and content type Excel needs to find them.
    #[test]
    fn wb_save_of_a_new_workbook_creates_core_properties() {
        let dir = std::env::temp_dir().join(format!("xlsxy-600-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("new.xlsx");
        let mut a = App::new(new_xlsx(), book.to_str().unwrap());
        a.os_clip = None;
        let before = iso_now();
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        let after = iso_now();
        let saved = load_xlsx(&std::fs::read(&book).unwrap()).unwrap();
        let p = saved.doc_properties();
        assert_eq!(p.creator, Some(comment_author()));
        assert_eq!(p.last_modified_by, Some(comment_author()));
        let created = p.created.unwrap();
        assert!(before <= created && created <= after);
        assert_eq!(p.modified, Some(created));
        assert!(part_text(&saved, "_rels/.rels").contains(
            r#"Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml""#
        ));
        assert!(part_text(&saved, "[Content_Types].xml").contains(
            r#"<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>"#
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn props(a: &mut App) -> Json {
        dispatch(a, "wb.properties", &Json::Null).unwrap()
    }

    #[test]
    fn wb_properties_reports_every_property_with_null_for_absent_ones() {
        let mut a = App::new(load_xlsx(&props_fixture()).unwrap(), "p.xlsx");
        a.os_clip = None;
        let p = props(&mut a);
        assert_eq!(p.get_str("author"), Some("Author One"));
        assert_eq!(p.get_str("lastModifiedBy"), Some("Author Two"));
        assert_eq!(p.get_str("created"), Some("2020-01-02T03:04:05Z"));
        assert_eq!(p.get_str("modified"), Some("2020-01-02T03:04:05Z"));
        for key in [
            "title",
            "tags",
            "categories",
            "subject",
            "comments",
            "company",
            "manager",
            "hyperlinkBase",
        ] {
            assert_eq!(p.get(key), Some(&Json::Null), "{key}");
        }
        assert_eq!(p.get("custom"), Some(&Json::Arr(vec![])));
    }

    #[test]
    fn wb_set_properties_sets_removes_and_marks_modified() {
        let mut a = app();
        assert!(!a.modified);
        let custom = Json::obj(vec![
            ("Client", Json::Str("Contoso".into())),
            ("Count", Json::Num(3.0)),
            ("Done", Json::Bool(true)),
            (
                "Due",
                Json::obj(vec![("date", Json::Str("2024-05-06".into()))]),
            ),
        ]);
        let r = dispatch(
            &mut a,
            "wb.set-properties",
            &Json::obj(vec![
                ("title", Json::Str("Budget".into())),
                ("tags", Json::Str("q3 plan".into())),
                ("categories", Json::Str("Finance".into())),
                ("subject", Json::Str("Spend".into())),
                ("comments", Json::Str("Draft & <notes>".into())),
                ("company", Json::Str("Acme".into())),
                ("manager", Json::Str("Pat".into())),
                ("hyperlinkBase", Json::Str("https://example.com/".into())),
                ("custom", custom),
                ("target", Json::Str("pane-1".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get("changed"), Some(&Json::Bool(true)));
        assert!(a.modified);
        assert_eq!(r.get_str("title"), Some("Budget"));
        assert_eq!(r.get_str("comments"), Some("Draft & <notes>"));
        assert_eq!(r.get_str("hyperlinkBase"), Some("https://example.com/"));
        let custom = r.get("custom").unwrap().as_array().unwrap();
        let entry = |i: usize| {
            (
                custom[i].get_str("name").unwrap().to_string(),
                custom[i].get_str("type").unwrap().to_string(),
                custom[i].get("value").unwrap().clone(),
            )
        };
        assert_eq!(
            entry(0),
            ("Client".into(), "text".into(), Json::Str("Contoso".into()))
        );
        assert_eq!(entry(1), ("Count".into(), "number".into(), Json::Num(3.0)));
        assert_eq!(entry(2), ("Done".into(), "bool".into(), Json::Bool(true)));
        assert_eq!(
            entry(3),
            (
                "Due".into(),
                "date".into(),
                Json::Str("2024-05-06T00:00:00Z".into())
            )
        );

        // Survives a save and reload.
        let re = load_xlsx(&save_xlsx(&a.pkg)).unwrap();
        let mut b = App::new(re, "re.xlsx");
        b.os_clip = None;
        let mut again = props(&mut b);
        if let Json::Obj(f) = &mut again {
            f.push(("changed".into(), Json::Bool(true)));
        }
        assert_eq!(again, r);

        // null and "" remove; a name in another case changes in place.
        a.modified = false;
        let r = dispatch(
            &mut a,
            "wb.set-properties",
            &Json::obj(vec![
                ("title", Json::Null),
                ("tags", Json::Str(String::new())),
                (
                    "custom",
                    Json::obj(vec![
                        ("client", Json::Str("Fabrikam".into())),
                        ("Count", Json::Null),
                        ("Done", Json::Str(String::new())),
                    ]),
                ),
            ]),
        )
        .unwrap();
        assert_eq!(r.get("changed"), Some(&Json::Bool(true)));
        assert!(a.modified);
        assert_eq!(r.get("title"), Some(&Json::Null));
        assert_eq!(r.get("tags"), Some(&Json::Null));
        assert_eq!(r.get_str("subject"), Some("Spend"));
        let names: Vec<&str> = r
            .get("custom")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.get_str("name").unwrap())
            .collect();
        assert_eq!(names, ["Client", "Due"]);
        assert_eq!(
            r.get("custom").unwrap().as_array().unwrap()[0].get_str("value"),
            Some("Fabrikam")
        );
    }

    #[test]
    fn wb_set_properties_no_op_leaves_the_workbook_unmodified() {
        let mut a = app();
        let args = Json::obj(vec![("title", Json::Str("T".into()))]);
        dispatch(&mut a, "wb.set-properties", &args).unwrap();
        a.modified = false;
        let r = dispatch(&mut a, "wb.set-properties", &args).unwrap();
        assert_eq!(r.get("changed"), Some(&Json::Bool(false)));
        assert!(!a.modified);
    }

    /// #600 r2: a property that can't land (UTF-16 core.xml) is an error,
    /// not a `changed:true` that wrote nothing.
    #[test]
    fn wb_set_properties_errors_when_core_xml_is_unreadable() {
        let mut pkg = load_xlsx(&props_fixture()).unwrap();
        let core = part_text(&pkg, "docProps/core.xml");
        let mut utf16 = vec![0xFF, 0xFE];
        for u in core.encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        pkg.set_part("docProps/core.xml", utf16.clone());
        let mut a = App::new(pkg, "u.xlsx");
        a.os_clip = None;
        let err = dispatch(
            &mut a,
            "wb.set-properties",
            &Json::obj(vec![
                ("title", Json::Str("T".into())),
                ("company", Json::Str("Co".into())),
            ]),
        )
        .unwrap_err();
        assert_eq!(
            err,
            "document properties can't be edited: docProps/core.xml is unreadable"
        );
        assert!(!a.modified);
        assert_eq!(a.pkg.part("docProps/core.xml").unwrap(), utf16.as_slice());
        assert_eq!(props(&mut a).get("company"), Some(&Json::Null));
        // app.xml is readable: a Company-only edit goes through.
        let r = dispatch(
            &mut a,
            "wb.set-properties",
            &Json::obj(vec![("company", Json::Str("Co".into()))]),
        )
        .unwrap();
        assert_eq!(r.get("changed"), Some(&Json::Bool(true)));
        assert_eq!(r.get_str("company"), Some("Co"));
    }

    #[test]
    fn wb_set_properties_rejects_bad_input_and_changes_nothing() {
        let mut a = app();
        let before = props(&mut a);
        for args in [
            Json::obj(vec![
                ("title", Json::Str("T".into())),
                ("titel", Json::Str("typo".into())),
            ]),
            Json::obj(vec![("title", Json::Num(1.0))]),
            Json::obj(vec![
                ("title", Json::Str("T".into())),
                (
                    "custom",
                    Json::obj(vec![(
                        "Due",
                        Json::obj(vec![("date", Json::Str("2024-02-30x".into()))]),
                    )]),
                ),
            ]),
            Json::obj(vec![(
                "custom",
                Json::obj(vec![("List", Json::Arr(vec![]))]),
            )]),
            Json::obj(vec![("custom", Json::obj(vec![(" ", Json::Num(1.0))]))]),
            Json::Null,
        ] {
            assert!(
                dispatch(&mut a, "wb.set-properties", &args).is_err(),
                "{args}"
            );
        }
        assert_eq!(props(&mut a), before);
        assert!(!a.modified);
    }
}

#[cfg(test)]
mod print_tests {
    use super::*;
    use gridcore::xlsx::new_xlsx;

    fn app() -> App {
        let mut a = App::new(new_xlsx(), "print-test.xlsx");
        a.os_clip = None;
        a
    }

    fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::obj(pairs)
    }

    fn s(v: &str) -> Json {
        Json::Str(v.into())
    }

    fn n(v: f64) -> Json {
        Json::Num(v)
    }

    fn call(a: &mut App, verb: &str, args: Json) -> Json {
        dispatch(a, verb, &args).unwrap_or_else(|e| panic!("{verb}: {e}"))
    }

    /// Numbers in A1:<cols × rows>.
    fn fill(a: &mut App, rows: u32, cols: u32) {
        let sheet = &mut a.pkg.workbook.sheets[a.sheet];
        for r in 0..rows {
            for c in 0..cols {
                sheet.set_cell(r, c, Cell::number(f64::from(r * 100 + c)));
            }
        }
    }

    /// The saved worksheet part of the first sheet (`xl/worksheets/sheet1.xml`
    /// in a new workbook).
    fn saved_sheet(a: &App, i: usize) -> String {
        let pkg = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&a.pkg)).unwrap();
        let part = format!("xl/worksheets/sheet{}.xml", i + 1);
        String::from_utf8_lossy(pkg.part(&part).unwrap()).into_owned()
    }

    fn saved_workbook(a: &App) -> String {
        let pkg = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&a.pkg)).unwrap();
        String::from_utf8_lossy(pkg.part("xl/workbook.xml").unwrap()).into_owned()
    }

    #[test]
    fn page_setup_reads_the_defaults_without_marking_anything() {
        let mut a = app();
        let r = call(&mut a, "page.setup", Json::Null);
        assert_eq!(r.get_str("orientation"), Some("default"));
        assert_eq!(r.get_usize("scale"), Some(100));
        assert_eq!(r.get_usize("fitToWidth"), Some(1));
        assert_eq!(r.get("firstPageNumber"), Some(&Json::Null));
        assert_eq!(r.get("printArea"), Some(&Json::Null));
        assert_eq!(
            r.get("margins").unwrap().get("left").unwrap().as_f64(),
            Some(0.7)
        );
        assert!(!a.modified);
    }

    #[test]
    fn page_setup_sets_named_attributes_and_undoes_in_one_step() {
        let mut a = app();
        let r = call(
            &mut a,
            "page.setup",
            obj(vec![
                ("orientation", s("landscape")),
                ("paperSize", n(9.0)),
                ("gridLines", Json::Bool(true)),
                ("margins", obj(vec![("left", n(1.0))])),
            ]),
        );
        assert_eq!(r.get("changed"), Some(&Json::Bool(true)));
        assert!(a.modified);
        let ws = saved_sheet(&a, 0);
        assert!(ws.contains(r#"<printOptions gridLines="1"/>"#), "{ws}");
        assert!(
            ws.contains(r#"<pageMargins left="1" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>"#),
            "{ws}"
        );
        assert!(
            ws.contains(r#"<pageSetup orientation="landscape" paperSize="9"/>"#)
                || ws.contains(r#"<pageSetup paperSize="9" orientation="landscape"/>"#),
            "{ws}"
        );
        a.undo();
        let r = call(&mut a, "page.setup", Json::Null);
        assert_eq!(r.get_str("orientation"), Some("default"));
        assert_eq!(r.get("gridLines"), Some(&Json::Bool(false)));
    }

    #[test]
    fn a_fit_count_turns_fit_on_and_a_scale_turns_it_off() {
        // FIL-CASE-044.
        let mut a = app();
        let r = call(&mut a, "page.setup", obj(vec![("fitToWidth", n(1.0))]));
        assert_eq!(r.get("fitToPage"), Some(&Json::Bool(true)));
        call(&mut a, "page.setup", obj(vec![("fitToWidth", n(0.0))]));
        let r = call(&mut a, "page.setup", obj(vec![("scale", n(150.0))]));
        assert_eq!(r.get("fitToPage"), Some(&Json::Bool(false)));
        let ws = saved_sheet(&a, 0);
        assert!(ws.contains(r#"scale="150""#), "{ws}");
        assert!(!ws.contains("fitToPage"), "{ws}");
    }

    #[test]
    fn out_of_range_scale_and_unknown_fields_are_refused_unchanged() {
        // FIL-CASE-040 step 3.
        let mut a = app();
        for bad in [401.0, 9.0] {
            let e = dispatch(&mut a, "page.setup", &obj(vec![("scale", n(bad))])).unwrap_err();
            assert!(e.contains("10–400"), "{e}");
        }
        let e = dispatch(&mut a, "page.setup", &obj(vec![("scael", n(50.0))])).unwrap_err();
        assert!(e.contains("unknown field 'scael'"), "{e}");
        let e = dispatch(
            &mut a,
            "page.setup",
            &obj(vec![("orientation", s("sideways"))]),
        )
        .unwrap_err();
        assert!(e.contains("sideways"), "{e}");
        assert!(!a.modified);
        assert_eq!(
            a.pkg.workbook.sheets[0].page_setup,
            gridcore::print::setup::PageSetup::default()
        );
        // Setting a value it already has changes nothing.
        let r = call(&mut a, "page.setup", obj(vec![("scale", n(100.0))]));
        assert_eq!(r.get("changed"), Some(&Json::Bool(false)));
        assert!(!a.modified);
    }

    #[test]
    fn the_mcp_target_key_is_not_a_page_setup_field() {
        // FIX r1 M2: the MCP bridge forwards `target` with the arguments.
        let mut a = app();
        let r = call(&mut a, "page.setup", obj(vec![("target", s("pane-1"))]));
        assert_eq!(r.get_str("orientation"), Some("default"));
        assert!(!a.modified);
        let r = call(
            &mut a,
            "page.setup",
            obj(vec![
                ("target", s("pane-1")),
                ("orientation", s("landscape")),
            ]),
        );
        assert_eq!(r.get("changed"), Some(&Json::Bool(true)));
        let target = || ("target", s("pane-1"));
        call(
            &mut a,
            "page.header",
            obj(vec![target(), ("center", s("x"))]),
        );
        call(
            &mut a,
            "print-area.set",
            obj(vec![target(), ("range", s("A1:B2"))]),
        );
        call(
            &mut a,
            "print-titles.set",
            obj(vec![target(), ("rows", s("1:1"))]),
        );
        call(
            &mut a,
            "page-break.insert",
            obj(vec![target(), ("cell", s("A5"))]),
        );
        call(&mut a, "print.pages", obj(vec![target()]));
    }

    #[test]
    fn a_hidden_current_sheet_still_prints() {
        // FIX r4 M2: the editor shows and edits hidden sheets.
        let mut a = app();
        fill(&mut a, 10, 2);
        a.pkg.workbook.sheets[0].hidden = true;
        let r = call(&mut a, "print.pages", Json::Null);
        assert_eq!(r.get_usize("total"), Some(1));
        let r = call(&mut a, "print.pages", obj(vec![("what", s("workbook"))]));
        assert_eq!(r.get_usize("total"), Some(0));
    }

    #[test]
    fn a_sheet_named_twice_prints_once() {
        // FIX r3 m3.
        let mut a = app();
        fill(&mut a, 10, 2);
        for sheets in [
            Json::Arr(vec![n(0.0), n(0.0)]),
            Json::Arr(vec![s("Sheet1"), s("sheet1")]),
        ] {
            let r = call(&mut a, "print.pages", obj(vec![("sheets", sheets.clone())]));
            assert_eq!(r.get_usize("total"), Some(1), "{sheets:?}");
        }
    }

    #[test]
    fn a_sheet_named_twice_in_a_group_keeps_its_own_header_picture() {
        // FIX r2 m6.
        let mut a = app();
        a.pkg.workbook.sheets[0].page_setup.header_footer.odd_header = Some("&L&G".into());
        for sheets in [
            Json::Arr(vec![n(0.0), n(0.0)]),
            Json::Arr(vec![s("Sheet1"), s("sheet1")]),
        ] {
            call(
                &mut a,
                "page.setup",
                obj(vec![("sheets", sheets), ("gridLines", Json::Bool(true))]),
            );
            assert_eq!(
                a.pkg.workbook.sheets[0]
                    .page_setup
                    .header_footer
                    .odd_header
                    .as_deref(),
                Some("&L&G")
            );
        }
    }

    #[test]
    fn a_job_past_the_page_limit_is_an_error_for_both_print_verbs() {
        // FIX r2 M1: XFD1048576 at 10 % is hundreds of thousands of pages.
        let mut a = app();
        a.pkg.workbook.sheets[0].set_cell(1_048_575, 16_383, Cell::number(1.0));
        call(&mut a, "page.setup", obj(vec![("scale", n(10.0))]));
        let e = dispatch(&mut a, "print.pages", &Json::Null).unwrap_err();
        assert!(e.contains("more than 100000 pages"), "{e}");
        let e = dispatch(&mut a, "print.pages", &obj(vec![("what", s("workbook"))])).unwrap_err();
        assert!(e.contains("more than 100000 pages"), "{e}");
        let out = std::env::temp_dir().join(format!("xlsxy-too-many-{}.pdf", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let e = dispatch(
            &mut a,
            "wb.export-pdf",
            &obj(vec![("path", s(&out.to_string_lossy()))]),
        )
        .unwrap_err();
        assert!(e.contains("more than 100000 pages"), "{e}");
        assert!(!out.exists());
    }

    #[test]
    fn grouped_sheets_take_the_first_sheets_setup_but_keep_their_print_areas() {
        let mut a = app();
        call(&mut a, "sheet.add", obj(vec![("name", s("Two"))]));
        a.sheet = 0;
        call(
            &mut a,
            "print-area.set",
            obj(vec![("range", s("A1:B2")), ("sheet", s("Two"))]),
        );
        call(
            &mut a,
            "page.header",
            obj(vec![("sheet", n(0.0)), ("center", s("Report"))]),
        );
        call(
            &mut a,
            "page.setup",
            obj(vec![
                ("sheets", Json::Arr(vec![n(0.0), s("Two")])),
                ("orientation", s("landscape")),
            ]),
        );
        let two = &a.pkg.workbook.sheets[1].page_setup;
        assert_eq!(two.orientation.as_str(), "landscape");
        assert_eq!(two.header_footer.odd_header.as_deref(), Some("&CReport"));
        let r = call(&mut a, "page.setup", obj(vec![("sheet", s("Two"))]));
        assert_eq!(r.get_str("printArea"), Some("A1:B2"));
    }

    #[test]
    fn header_fields_typed_in_editor_form_are_stored_as_excel_codes() {
        // FIL-CASE-041.
        let mut a = app();
        let r = call(
            &mut a,
            "page.header",
            obj(vec![
                ("center", s("R&&D &[Page] of &[Pages]")),
                ("right", s("&[Tab]")),
            ]),
        );
        assert_eq!(r.get_str("stored"), Some("&CR&&D &P of &N&R&A"));
        let r = call(
            &mut a,
            "page.header",
            obj(vec![("part", s("footer")), ("left", s("&[Path]&[File]"))]),
        );
        assert_eq!(r.get_str("stored"), Some("&L&Z&F"));
        let ws = saved_sheet(&a, 0);
        assert!(
            ws.contains("<headerFooter><oddHeader>&amp;CR&amp;&amp;D &amp;P of &amp;N&amp;R&amp;A</oddHeader><oddFooter>&amp;L&amp;Z&amp;F</oddFooter></headerFooter>"),
            "{ws}"
        );
        // Reading gives the editor form back.
        let r = call(&mut a, "page.header", Json::Null);
        assert_eq!(r.get_str("center"), Some("R&&D &[Page] of &[Pages]"));
        assert_eq!(r.get_str("right"), Some("&[Tab]"));
        assert_eq!(r.get("changed"), Some(&Json::Bool(false)));
        // Clearing every section removes it.
        call(
            &mut a,
            "page.header",
            obj(vec![("part", s("footer")), ("left", s(""))]),
        );
        assert_eq!(
            a.pkg.workbook.sheets[0].page_setup.header_footer.odd_footer,
            None
        );
    }

    #[test]
    fn a_header_picture_can_be_kept_but_not_added() {
        let mut a = app();
        let e = dispatch(&mut a, "page.header", &obj(vec![("left", s("&[Picture]"))])).unwrap_err();
        assert!(e.contains("&[Picture]"), "{e}");
        assert!(!a.modified);
        a.pkg.workbook.sheets[0].page_setup.header_footer.odd_header = Some("&L&G".into());
        let r = call(
            &mut a,
            "page.header",
            obj(vec![("left", s("&[Picture]")), ("right", s("x"))]),
        );
        assert_eq!(r.get_str("stored"), Some("&L&G&Rx"));
        // FIX r1 m7: the picture belongs to its section; another can't take it.
        let e = dispatch(
            &mut a,
            "page.header",
            &obj(vec![("left", s("&[Picture]")), ("right", s("&[Picture]"))]),
        )
        .unwrap_err();
        assert!(e.contains("right section"), "{e}");
        let e = dispatch(
            &mut a,
            "page.header",
            &obj(vec![("right", s("&[Picture]"))]),
        )
        .unwrap_err();
        assert!(e.contains("right section"), "{e}");
        // FIX r2 m5: a section code can't smuggle a picture across.
        let e = dispatch(
            &mut a,
            "page.header",
            &obj(vec![("left", s("&[Picture]&R&[Picture]"))]),
        )
        .unwrap_err();
        assert!(e.contains("starts a section"), "{e}");
        assert_eq!(
            a.pkg.workbook.sheets[0]
                .page_setup
                .header_footer
                .odd_header
                .as_deref(),
            Some("&L&G&Rx")
        );
        let long = "x".repeat(256);
        assert!(dispatch(&mut a, "page.header", &obj(vec![("left", s(&long))])).is_err());
    }

    #[test]
    fn print_area_set_add_and_clear() {
        // FIL-CASE-045.
        let mut a = app();
        fill(&mut a, 40, 10);
        let r = call(&mut a, "print-area.set", obj(vec![("range", s("A1:C10"))]));
        assert_eq!(r.get_str("printArea"), Some("Sheet1!$A$1:$C$10"));
        call(&mut a, "print-area.add", obj(vec![("range", s("E1:F5"))]));
        let wb = saved_workbook(&a);
        assert!(
            wb.contains(r#"<definedName name="_xlnm.Print_Area" localSheetId="0">Sheet1!$A$1:$C$10,Sheet1!$E$1:$F$5</definedName>"#),
            "{wb}"
        );
        let r = call(&mut a, "print-area.clear", Json::Null);
        assert_eq!(r.get("printArea"), Some(&Json::Null));
        assert!(!saved_workbook(&a).contains("Print_Area"));
        a.undo();
        assert!(saved_workbook(&a).contains("Print_Area"));
        let e = dispatch(
            &mut a,
            "print-area.set",
            &obj(vec![("range", s("A1:C10,bogus"))]),
        )
        .unwrap_err();
        assert!(e.contains("bad range"), "{e}");
    }

    #[test]
    fn print_titles_set_and_clear_each_part() {
        let mut a = app();
        let r = call(
            &mut a,
            "print-titles.set",
            obj(vec![("rows", s("1:2")), ("cols", s("A:A"))]),
        );
        let t = r.get("printTitles").unwrap();
        assert_eq!(
            (t.get_str("rows"), t.get_str("cols")),
            (Some("1:2"), Some("A:A"))
        );
        assert!(saved_workbook(&a).contains(
            r#"<definedName name="_xlnm.Print_Titles" localSheetId="0">Sheet1!$A:$A,Sheet1!$1:$2</definedName>"#
        ));
        let r = call(&mut a, "print-titles.set", obj(vec![("cols", Json::Null)]));
        let t = r.get("printTitles").unwrap();
        assert_eq!(
            (t.get_str("rows"), t.get("cols")),
            (Some("1:2"), Some(&Json::Null))
        );
        let e = dispatch(&mut a, "print-titles.set", &obj(vec![("rows", s("A:A"))])).unwrap_err();
        assert!(e.contains("whole rows"), "{e}");
    }

    #[test]
    fn page_breaks_insert_remove_reset_and_undo() {
        // FIL-CASE-046.
        let mut a = app();
        fill(&mut a, 60, 8);
        for cell in ["A14", "D1", "F30"] {
            call(&mut a, "page-break.insert", obj(vec![("cell", s(cell))]));
        }
        let ws = saved_sheet(&a, 0);
        assert!(
            ws.contains(r#"<rowBreaks count="2" manualBreakCount="2"><brk id="13" max="16383" man="1"/><brk id="29" max="16383" man="1"/></rowBreaks><colBreaks count="2" manualBreakCount="2"><brk id="3" max="1048575" man="1"/><brk id="5" max="1048575" man="1"/></colBreaks>"#),
            "{ws}"
        );
        let r = call(&mut a, "page-break.remove", obj(vec![("cell", s("F30"))]));
        assert_eq!(r.get("rowBreaks").unwrap().to_string(), "[13]");
        assert_eq!(r.get("colBreaks").unwrap().to_string(), "[3]");
        let r = call(&mut a, "page-break.reset", Json::Null);
        assert_eq!(r.get("changed"), Some(&Json::Bool(true)));
        let ws = saved_sheet(&a, 0);
        assert!(!ws.contains("Breaks"), "{ws}");
        a.undo();
        assert_eq!(
            gridcore::print::area::manual_breaks(&a.pkg.workbook.sheets[0]),
            (vec![13], vec![3])
        );
        let r = call(&mut a, "page-break.insert", obj(vec![("cell", s("A1"))]));
        assert_eq!(r.get("changed"), Some(&Json::Bool(false)));
    }

    #[test]
    fn print_pages_reports_each_pages_range_titles_and_number() {
        // FIL-CASE-042's shape.
        let mut a = app();
        fill(&mut a, 200, 13);
        call(
            &mut a,
            "print-titles.set",
            obj(vec![("rows", s("1:1")), ("cols", s("A:A"))]),
        );
        call(
            &mut a,
            "print-area.set",
            obj(vec![("range", s("A1:F100,H1:M50"))]),
        );
        call(
            &mut a,
            "page.setup",
            obj(vec![
                ("pageOrder", s("overThenDown")),
                ("fitToWidth", n(1.0)),
                ("fitToHeight", n(0.0)),
            ]),
        );
        let r = call(&mut a, "print.pages", Json::Null);
        let pages = r.get("pages").unwrap().as_array().unwrap();
        assert_eq!(r.get_usize("total"), Some(pages.len()));
        assert_eq!(pages[0].get_str("range"), Some("A1:F45"));
        assert_eq!(pages[0].get("titleRows"), Some(&Json::Null));
        assert_eq!(pages[1].get_str("titleRows"), Some("1:1"));
        // The second range starts a page of its own, with column A repeated.
        let h = pages
            .iter()
            .find(|p| p.get_str("range").is_some_and(|r| r.starts_with("H1")))
            .unwrap();
        assert_eq!(h.get_str("titleCols"), Some("A:A"));
        let r = call(
            &mut a,
            "print.pages",
            obj(vec![
                ("ignorePrintAreas", Json::Bool(true)),
                ("from", n(2.0)),
                ("to", n(2.0)),
            ]),
        );
        assert_eq!(r.get("pages").unwrap().as_array().unwrap().len(), 1);
        let r = call(
            &mut a,
            "print.pages",
            obj(vec![("what", s("selection")), ("range", s("B2:C3,E5"))]),
        );
        let pages = r.get("pages").unwrap().as_array().unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[1].get_str("range"), Some("E5"));
        assert!(dispatch(&mut a, "print.pages", &obj(vec![("what", s("chart"))])).is_err());
        assert!(dispatch(&mut a, "print.pages", &obj(vec![("what", s("selection"))])).is_err());
    }

    #[test]
    fn export_pdf_writes_once_and_refuses_an_empty_sheet() {
        // FIL-CASE-043.
        let dir = std::env::temp_dir().join(format!("xlsxy-pdf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = app();
        fill(&mut a, 100, 13);
        call(
            &mut a,
            "page.header",
            obj(vec![("center", s("Page &[Page] of &[Pages]"))]),
        );
        call(&mut a, "sheet.add", obj(vec![("name", s("Empty"))]));
        a.sheet = 0;
        let out = dir.join("out.pdf");
        let path = out.to_string_lossy().into_owned();
        let r = call(&mut a, "wb.export-pdf", obj(vec![("path", s(&path))]));
        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        let text = String::from_utf8_lossy(&bytes);
        let pages = r.get_usize("pages").unwrap();
        assert!(text.contains(&format!("(Page 1 of {pages}) Tj")), "header");
        assert!(text.contains(&format!("/Count {pages}")));
        let e = dispatch(&mut a, "wb.export-pdf", &obj(vec![("path", s(&path))])).unwrap_err();
        assert!(e.starts_with("already exists"), "{e}");
        let empty = dir.join("empty.pdf");
        let e = dispatch(
            &mut a,
            "wb.export-pdf",
            &obj(vec![
                ("path", s(&empty.to_string_lossy())),
                ("sheet", s("Empty")),
            ]),
        )
        .unwrap_err();
        assert_eq!(e, "We didn't find anything to print.");
        assert!(!empty.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod table_verb_tests {
    use super::*;
    use gridcore::xlsx::new_xlsx;

    /// Item/Qty over A1:B3 as `Table1`, `=SUM(Table1[Qty])` in D1.
    fn app() -> App {
        let mut a = App::new(new_xlsx(), "ctl-table.xlsx");
        a.os_clip = None;
        let sh = &mut a.pkg.workbook.sheets[0];
        sh.set_cell(0, 0, Cell::text("Item"));
        sh.set_cell(0, 1, Cell::text("Qty"));
        sh.set_cell(1, 0, Cell::text("Pen"));
        sh.set_cell(1, 1, Cell::number(3.0));
        sh.set_cell(2, 0, Cell::text("Pad"));
        sh.set_cell(2, 1, Cell::number(5.0));
        a.pkg
            .add_table(0, (0, 0, 2, 1), true, "TableStyleMedium2")
            .unwrap();
        a.rebuild_engine();
        cell_set(
            &mut a,
            &Json::obj(vec![
                ("ref", Json::Str("D1".into())),
                ("text", Json::Str("=SUM(Table1[Qty])".into())),
            ]),
        )
        .unwrap();
        a
    }

    fn call(a: &mut App, verb: &str, args: Vec<(&str, &str)>) -> Result<Json, String> {
        let args = Json::obj(
            args.into_iter()
                .map(|(k, v)| (k, Json::Str(v.into())))
                .collect(),
        );
        dispatch(a, verb, &args)
    }

    fn d1(a: &App) -> Option<String> {
        cell_get(a, &Json::obj(vec![("ref", Json::Str("D1".into()))]))
            .unwrap()
            .get_str("formula")
            .map(str::to_string)
    }

    #[test]
    fn table_list_describes_each_table() {
        let mut a = app();
        let r = dispatch(&mut a, "table.list", &Json::Null).unwrap();
        let tables = r.get("tables").unwrap().as_array().unwrap();
        assert_eq!(tables.len(), 1);
        let t = &tables[0];
        assert_eq!(t.get_str("name"), Some("Table1"));
        assert_eq!(t.get_usize("sheet"), Some(0));
        assert_eq!(t.get_str("sheet_name"), Some("Sheet1"));
        assert_eq!(t.get_str("ref"), Some("A1:B3"));
        let cols: Vec<&str> = t
            .get("columns")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c.as_str())
            .collect();
        assert_eq!(cols, vec!["Item", "Qty"]);
        assert_eq!(t.get_usize("header_rows"), Some(1));
        assert_eq!(t.get_usize("totals_rows"), Some(0));
    }

    #[test]
    fn table_rename_rewrites_formulas_and_undoes_in_one_step() {
        let mut a = app();
        let r = call(
            &mut a,
            "table.rename",
            vec![("name", "table1"), ("new", "Sales")],
        )
        .unwrap();
        assert_eq!(r.get_str("name"), Some("Sales"));
        assert_eq!(d1(&a).as_deref(), Some("=SUM(Sales[Qty])"));
        a.undo();
        assert_eq!(a.pkg.workbook.tables[0].name, "Table1");
        assert_eq!(d1(&a).as_deref(), Some("=SUM(Table1[Qty])"));
        let err = call(
            &mut a,
            "table.rename",
            vec![("name", "Table1"), ("new", "R1C1")],
        )
        .unwrap_err();
        assert!(err.contains("cell reference"), "{err}");
        let err = call(&mut a, "table.rename", vec![("name", "Nope"), ("new", "X")]).unwrap_err();
        assert_eq!(err, "There is no table named Nope");
        assert!(call(&mut a, "table.rename", vec![("name", "Table1")]).is_err());
    }

    #[test]
    fn table_resize_moves_the_table() {
        let mut a = app();
        let r = call(
            &mut a,
            "table.resize",
            vec![("name", "Table1"), ("ref", "A1:C5")],
        )
        .unwrap();
        assert_eq!(r.get_str("ref"), Some("A1:C5"));
        assert_eq!(r.get("columns").unwrap().as_array().unwrap().len(), 3);
        let err = call(
            &mut a,
            "table.resize",
            vec![("name", "Table1"), ("ref", "F1:G4")],
        )
        .unwrap_err();
        assert_eq!(err, "The new range must overlap the table");
        a.undo();
        assert_eq!(a.pkg.workbook.tables[0].range, (0, 0, 2, 1));
    }

    #[test]
    fn table_convert_writes_cell_references() {
        let mut a = app();
        let r = call(&mut a, "table.convert", vec![("name", "Table1")]).unwrap();
        assert_eq!(r.get_str("converted"), Some("Table1"));
        assert!(a.pkg.workbook.tables.is_empty());
        assert_eq!(d1(&a).as_deref(), Some("=SUM($B$2:$B$3)"));
        let err = call(&mut a, "table.convert", vec![("name", "Table1")]).unwrap_err();
        assert_eq!(err, "There is no table named Table1");
        a.undo();
        assert_eq!(a.pkg.workbook.tables.len(), 1);
        assert_eq!(d1(&a).as_deref(), Some("=SUM(Table1[Qty])"));
    }

    /// #683: `name` over A1 with `columns` as headers and `rows` data rows
    /// (column k of row r holds `10 * k + r`).
    fn app_683(name: &str, columns: &[&str], rows: u32) -> App {
        let mut a = App::new(new_xlsx(), "ctl-683.xlsx");
        a.os_clip = None;
        let sh = &mut a.pkg.workbook.sheets[0];
        for (k, col) in columns.iter().enumerate() {
            sh.set_cell(0, k as u32, Cell::text(col));
            for r in 1..=rows {
                sh.set_cell(r, k as u32, Cell::number((10 * k as u32 + r) as f64));
            }
        }
        let last = columns.len() as u32 - 1;
        a.pkg
            .add_table(0, (0, 0, rows, last), true, "TableStyleMedium2")
            .unwrap();
        gridcore::edit::rename_table(&mut a.pkg.workbook, "Table1", name).unwrap();
        a.rebuild_engine();
        a
    }

    fn put(a: &mut App, at: &str, text: &str) {
        call(a, "cell.set", vec![("ref", at), ("text", text)]).unwrap();
    }

    fn formula_value(a: &mut App, at: &str) -> (Option<String>, Option<String>) {
        let g = call(a, "cell.get", vec![("ref", at)]).unwrap();
        (
            g.get_str("formula").map(str::to_string),
            g.get_str("text").map(str::to_string),
        )
    }

    fn table_columns(a: &mut App) -> Vec<String> {
        let r = dispatch(a, "table.list", &Json::Null).unwrap();
        let t = &r.get("tables").unwrap().as_array().unwrap()[0];
        let cols = t.get("columns").unwrap().as_array().unwrap();
        cols.iter()
            .filter_map(|c| c.as_str())
            .map(str::to_string)
            .collect()
    }

    fn col_delete(a: &mut App, at: usize, count: usize) {
        let args = Json::obj(vec![
            ("at", Json::Num(at as f64)),
            ("count", Json::Num(count as f64)),
        ]);
        dispatch(a, "col.delete", &args).unwrap();
    }

    #[test]
    fn header_rename_rewrites_structured_refs_683() {
        let mut a = app_683("Sales", &["Item", "Qty", "Price", "Region"], 4);
        put(&mut a, "F2", "=SUM(Sales[Qty])");
        let (_, before) = formula_value(&mut a, "F2");
        assert_eq!(before.as_deref(), Some("50"));
        put(&mut a, "B1", "Units");
        let (f, v) = formula_value(&mut a, "F2");
        assert_eq!(f.as_deref(), Some("=SUM(Sales[Units])"));
        assert_eq!(v, before);
        assert_eq!(table_columns(&mut a), ["Item", "Units", "Price", "Region"]);
    }

    #[test]
    fn col_delete_refs_to_a_deleted_column_go_ref_683() {
        let mut a = app_683("Sales", &["Item", "Qty", "Price", "Region"], 4);
        put(&mut a, "F2", "=SUM(Sales[Qty])");
        put(&mut a, "G2", "=SUM(Sales[Price])");
        put(&mut a, "B1", "Units");
        col_delete(&mut a, 2, 1);
        assert_eq!(
            formula_value(&mut a, "F2"),
            (Some("=SUM(#REF!)".into()), Some("#REF!".into()))
        );
        assert_eq!(
            formula_value(&mut a, "E2"),
            (Some("=SUM(Sales[Units])".into()), Some("50".into()))
        );
        let t = &a.pkg.workbook.tables[0];
        assert_eq!(t.range, (0, 0, 4, 2));
        assert_eq!(table_columns(&mut a), ["Item", "Units", "Region"]);
        // One undo brings the column, the table and the formulas back.
        a.undo();
        assert_eq!(table_columns(&mut a), ["Item", "Units", "Price", "Region"]);
        assert_eq!(
            formula_value(&mut a, "G2"),
            (Some("=SUM(Sales[Price])".into()), Some("90".into()))
        );
    }

    #[test]
    fn replace_all_through_a_header_renames_the_column_683() {
        let mut a = app_683("Sales", &["Item", "Qty", "Price", "Region"], 4);
        put(&mut a, "F2", "=SUM(Sales[Qty])");
        put(&mut a, "G2", "=SUM(Sales[Price])");
        call(
            &mut a,
            "wb.replace-all",
            vec![("query", "Qty"), ("text", "Units")],
        )
        .unwrap();
        assert_eq!(table_columns(&mut a), ["Item", "Units", "Price", "Region"]);
        assert_eq!(
            formula_value(&mut a, "F2"),
            (Some("=SUM(Sales[Units])".into()), Some("50".into()))
        );
        // A header it didn't write keeps its column, and its name.
        assert_eq!(
            formula_value(&mut a, "G2"),
            (Some("=SUM(Sales[Price])".into()), Some("90".into()))
        );
        a.undo();
        assert_eq!(table_columns(&mut a), ["Item", "Qty", "Price", "Region"]);
        assert_eq!(
            formula_value(&mut a, "F2"),
            (Some("=SUM(Sales[Qty])".into()), Some("50".into()))
        );
    }

    #[test]
    fn col_insert_inside_a_table_renames_nothing_683() {
        let mut a = app_683("Sales", &["Item", "Qty", "Price", "Region"], 4);
        put(&mut a, "F2", "=SUM(Sales[Price])");
        put(&mut a, "G2", "=COUNTA(Sales[Region])");
        let args = Json::obj(vec![("at", Json::Num(2.0))]);
        dispatch(&mut a, "col.insert", &args).unwrap();
        assert_eq!(table_columns(&mut a), ["Item", "Qty", "Price", "Region"]);
        assert_eq!(
            formula_value(&mut a, "G2").0.as_deref(),
            Some("=SUM(Sales[Price])")
        );
        assert_eq!(
            formula_value(&mut a, "H2").0.as_deref(),
            Some("=COUNTA(Sales[Region])")
        );
    }

    #[test]
    fn col_delete_of_a_whole_table_removes_it_on_save_683() {
        let tmp = std::env::temp_dir().join(format!("xlsxy-683-{}.xlsx", std::process::id()));
        let mut a = app_683("Calc", &["Qty", "Price", "Line"], 3);
        a.path = tmp.to_string_lossy().into_owned();
        put(&mut a, "F2", "=SUM(Calc[Line])");
        col_delete(&mut a, 0, 3);
        assert!(a.pkg.workbook.tables.is_empty());
        let ref_err = (Some("=SUM(#REF!)".to_string()), Some("#REF!".to_string()));
        assert_eq!(formula_value(&mut a, "C2"), ref_err);
        dispatch(&mut a, "wb.save", &Json::Null).unwrap();
        let re = gridcore::xlsx::load_xlsx(&std::fs::read(&tmp).unwrap()).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert!(re.workbook.tables.is_empty());
        assert!(re.part("xl/tables/table1.xml").is_none());
        let ws = String::from_utf8(re.part("xl/worksheets/sheet1.xml").unwrap().to_vec()).unwrap();
        assert!(!ws.contains("tablePart"), "{ws}");
        let rels = re
            .part("xl/worksheets/_rels/sheet1.xml.rels")
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .unwrap_or_default();
        assert!(!rels.contains("/table\""), "{rels}");
        // The reloaded cell still says #REF!, not #CYCLE!.
        let mut b = App::new(re, "re-683.xlsx");
        b.os_clip = None;
        b.rebuild_engine();
        assert_eq!(formula_value(&mut b, "C2"), ref_err);
        // Undo brings the table back.
        a.undo();
        assert_eq!(a.pkg.workbook.tables.len(), 1);
        assert_eq!(
            formula_value(&mut a, "F2"),
            (Some("=SUM(Calc[Line])".into()), Some("66".into()))
        );
    }
}

#[cfg(test)]
mod consolidate_tests {
    use super::*;
    use gridcore::xlsx::new_xlsx;

    fn app() -> App {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].name = "East".into();
        pkg.add_sheet("West");
        pkg.add_sheet("Summary");
        for (s, v) in [(0, 1.0), (1, 10.0)] {
            let sh = &mut pkg.workbook.sheets[s];
            sh.set_cell(0, 0, Cell::text("k"));
            sh.set_cell(0, 1, Cell::number(v));
        }
        let mut a = App::new(pkg, "Book.xlsx");
        a.os_clip = None;
        a
    }

    fn args(pairs: Vec<(&str, Json)>) -> Json {
        Json::obj(pairs)
    }

    fn refs(r: &[&str]) -> Json {
        Json::Arr(r.iter().map(|s| Json::Str(s.to_string())).collect())
    }

    fn value(a: &App, sheet: usize, r: u32, c: u32) -> Option<CellValue> {
        a.pkg.workbook.sheets[sheet]
            .cell(r, c)
            .map(|c| c.value.clone())
    }

    #[test]
    fn wb_consolidate_writes_at_dest_as_one_undo_step() {
        let mut a = app();
        let undo_len = a.undo.len();
        let r = dispatch(
            &mut a,
            "wb.consolidate",
            &args(vec![
                ("refs", refs(&["East!A1:B1", "West!A1:B1"])),
                ("dest", Json::Str("Summary!C3".into())),
                ("fn", Json::Str("count numbers".into())),
                ("left", Json::Bool(true)),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_str("sheet"), Some("Summary"));
        assert_eq!(r.get_str("range"), Some("C3:D3"));
        assert_eq!(value(&a, 2, 2, 2), Some(CellValue::Text("k".into())));
        assert_eq!(value(&a, 2, 2, 3), Some(CellValue::Number(2.0)));
        assert_eq!(a.undo.len(), undo_len + 1);
        // The file token works too.
        dispatch(
            &mut a,
            "wb.consolidate",
            &args(vec![
                ("refs", refs(&["East!A1:B1", "West!A1:B1"])),
                ("dest", Json::Str("Summary!C3".into())),
                ("fn", Json::Str("countNums".into())),
                ("left", Json::Bool(true)),
            ]),
        )
        .unwrap();
        assert_eq!(value(&a, 2, 2, 3), Some(CellValue::Number(2.0)));
        assert_eq!(
            a.pkg.workbook.sheets[2]
                .consolidate
                .as_ref()
                .map(|c| c.func),
            Some(gridcore::edit::SubtotalFunc::CountNums)
        );
        // Without `fn` it is Sum.
        dispatch(
            &mut a,
            "wb.consolidate",
            &args(vec![
                ("refs", refs(&["East!B1", "West!B1"])),
                ("dest", Json::Str("Summary!A1".into())),
            ]),
        )
        .unwrap();
        assert_eq!(value(&a, 2, 0, 0), Some(CellValue::Number(11.0)));
    }

    #[test]
    fn wb_consolidate_follows_the_destination_sheets_protection() {
        let mut a = app();
        // The active sheet is protected, the destination is not: allowed.
        a.sheet = 0;
        a.pkg.workbook.sheets[0].set_protected(true);
        let call = |a: &mut App| {
            dispatch(
                a,
                "wb.consolidate",
                &args(vec![
                    ("refs", refs(&["East!B1", "West!B1"])),
                    ("dest", Json::Str("Summary!A1".into())),
                ]),
            )
        };
        call(&mut a).unwrap();
        assert_eq!(value(&a, 2, 0, 0), Some(CellValue::Number(11.0)));
        // A protected destination refuses, whatever the active sheet.
        a.pkg.workbook.sheets[0].set_protected(false);
        a.pkg.workbook.sheets[2].set_protected(true);
        let undo_len = a.undo.len();
        let err = call(&mut a).unwrap_err();
        assert!(err.contains("protected"), "{err}");
        assert_eq!(a.undo.len(), undo_len);
    }

    #[test]
    fn wb_consolidate_refusals_change_nothing() {
        let mut a = app();
        let before = a.pkg.workbook.sheets.clone();
        let undo_len = a.undo.len();
        let err = |a: &mut App, pairs: Vec<(&str, Json)>| {
            dispatch(a, "wb.consolidate", &args(pairs)).unwrap_err()
        };
        assert!(err(&mut a, vec![]).contains("needs 'refs'"));
        assert!(
            err(
                &mut a,
                vec![
                    ("refs", refs(&["East!A1:B1"])),
                    ("fn", Json::Str("median".into()))
                ]
            )
            .contains("unknown function")
        );
        let e = err(
            &mut a,
            vec![
                ("refs", refs(&["East!A1:B1", "Nowhere!A1"])),
                ("dest", Json::Str("Summary!A1".into())),
            ],
        );
        assert!(e.contains("Nowhere!A1"), "{e}");
        let e = err(
            &mut a,
            vec![
                ("refs", refs(&["East!A1:B1", "West!A1:B1"])),
                ("dest", Json::Str("West!A5".into())),
                ("links", Json::Bool(true)),
            ],
        );
        assert_eq!(
            e,
            gridcore::edit::ConsolidateError::LinksOnDestSheet.to_string()
        );
        assert!(!gridcore::edit::sheets_differ(
            &before,
            &a.pkg.workbook.sheets
        ));
        assert_eq!(a.undo.len(), undo_len);
    }
}
