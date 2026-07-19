//! xlsxy's [MCP](https://modelcontextprotocol.io) stdio server: exposes the
//! control verbs as native tools for an MCP client such as Claude Code
//! (`claude mcp add xlsxy -- xlsxy --mcp`).
//!
//! A thin adapter over a running xlsxy's control surface (via
//! [`ctlcore::client`]); the protocol scaffolding lives in [`ctlcore::mcp`].
//! The MCP process opens no workbook of its own — it finds the xlsxy the user
//! already has open and forwards tool calls to it, so edits land on that
//! editor's live workbook and undo stack.

use ctlcore::client;
use ctlcore::json::Json;
use ctlcore::mcp::{McpServer, item_array, item_obj, item_ty, prop, prop_array, prop_obj, tool};

/// Serve MCP over stdio until stdin closes.
pub fn run() -> std::io::Result<()> {
    McpServer {
        name: "xlsxy",
        version: env!("CARGO_PKG_VERSION"),
        tools: tool_defs(),
        handler: &do_tool,
    }
    .run()
}

/// Map a forwarding tool name to its exact ctl verb string — the single
/// source of truth `do_tool` dispatches through, so a test can pin every
/// tool's verb precisely (not just "resolves to *something*", which a
/// swapped-but-valid mapping would still pass). Returns `None` for
/// `xlsxy_list`/`xlsxy_new` (handled specially in `do_tool`, not simple
/// forwards) and for any unrecognized name.
pub(crate) fn verb_for(name: &str) -> Option<&'static str> {
    Some(match name {
        "xlsxy_status" => "wb.path",
        "xlsxy_sheets" => "sheet.list",
        "xlsxy_read" => "sheet.read",
        "xlsxy_get" => "cell.get",
        "xlsxy_set" => "cell.set",
        "xlsxy_clear" => "range.clear",
        "xlsxy_find" => "find",
        "xlsxy_recalc" => "wb.recalc",
        "xlsxy_save" => "wb.save",
        "xlsxy_comments" => "comment.list",
        "xlsxy_comment_add" => "comment.add",
        "xlsxy_comment_remove" => "comment.remove",
        "xlsxy_range_set" => "range.set",
        "xlsxy_export_csv" => "wb.export-csv",
        "xlsxy_import_csv" => "sheet.import-csv",
        "xlsxy_pivot" => "sheet.pivot",
        "xlsxy_replace_all" => "wb.replace-all",
        "xlsxy_sheet_add" => "sheet.add",
        "xlsxy_sheet_remove" => "sheet.remove",
        "xlsxy_sheet_rename" => "sheet.rename",
        "xlsxy_row_insert" => "row.insert",
        "xlsxy_row_delete" => "row.delete",
        "xlsxy_col_insert" => "col.insert",
        "xlsxy_col_delete" => "col.delete",
        "xlsxy_eval" => "formula.eval",
        "xlsxy_stats" => "sheet.stats",
        "xlsxy_charts" => "chart.list",
        "xlsxy_pivots" => "pivot.list",
        "xlsxy_format" => "cell.format",
        "xlsxy_col_width" => "col.width",
        "xlsxy_pivot_create" => "pivot.create",
        _ => return None,
    })
}

/// Execute a tool by forwarding to the control surface.
fn do_tool(name: &str, args: &Json) -> Result<String, String> {
    let dir = ctlcore::config_ctl_dir("xlsxy").ok_or("no control directory on this system")?;
    if name == "xlsxy_list" {
        return Ok(client::list_running(&dir, "xlsxy").to_string());
    }
    if name == "xlsxy_new" {
        return Ok(
            client::new_file(&dir, "xlsxy", "wb.open", &blank_xlsx_bytes(), args)?.to_string(),
        );
    }
    let verb = verb_for(name).ok_or_else(|| format!("unknown tool: {name}"))?;
    let client = client::resolve_target(&dir, "xlsxy", args.get_str("target"))?;
    let result = client.call(verb, args.clone())?;
    Ok(result.to_string())
}

const TARGET_DESC: &str =
    "Optional: which xlsxy to act on (a substring of its instance/pane id) when several are open.";
const SHEET_DESC: &str = "Optional sheet index or name (default: the active sheet).";

/// A minimal valid .xlsx: one empty sheet ("Sheet1") in a fresh OPC package.
/// Also the source of the committed template the bundled VS Code MCP server ships.
pub(crate) fn blank_xlsx_bytes() -> Vec<u8> {
    gridcore::xlsx::save_xlsx(&gridcore::xlsx::new_xlsx())
}

fn tool_defs() -> Json {
    let target = || ("target", prop("string", TARGET_DESC));
    let sheet = || ("sheet", prop("string", SHEET_DESC));
    Json::Arr(vec![
        tool(
            "xlsxy_list",
            "List the xlsxy editors currently running on this machine (instance/pane id, port, pid).",
            vec![],
            &[],
        ),
        tool(
            "xlsxy_new",
            "Create a new blank .xlsx at a path and open it in the running xlsxy (in a VS Code \
             window, a new tab). With no xlsxy running the file is still created. Refuses to \
             overwrite an existing file.",
            vec![
                (
                    "path",
                    prop(
                        "string",
                        "File path for the new workbook (created; must not exist).",
                    ),
                ),
                target(),
            ],
            &["path"],
        ),
        tool(
            "xlsxy_status",
            "Report the open workbook's path, modified flag, sheet count, and active sheet.",
            vec![target()],
            &[],
        ),
        tool(
            "xlsxy_sheets",
            "List every sheet: index, name, and used size (rows/cols).",
            vec![target()],
            &[],
        ),
        tool(
            "xlsxy_read",
            "Read non-empty cells of the live workbook (including unsaved edits): value, formula, \
             and display text per cell. Defaults to the active sheet's whole used range, or pass \
             an A1-style range.",
            vec![
                ("range", prop("string", "A1-style range, e.g. \"A1:C10\".")),
                sheet(),
                target(),
            ],
            &[],
        ),
        tool(
            "xlsxy_get",
            "Read one cell: value, formula, and display text.",
            vec![
                ("ref", prop("string", "Cell reference, e.g. \"B4\".")),
                sheet(),
                target(),
            ],
            &["ref"],
        ),
        tool(
            "xlsxy_set",
            "Set a cell. A leading '=' makes a formula (validated + recalculated); otherwise \
             number/bool/text is inferred like typing into the grid. Undoable.",
            vec![
                ("ref", prop("string", "Cell reference, e.g. \"B4\".")),
                (
                    "text",
                    prop("string", "What to enter, e.g. \"42\" or \"=SUM(B1:B3)\"."),
                ),
                sheet(),
                target(),
            ],
            &["ref", "text"],
        ),
        tool(
            "xlsxy_clear",
            "Clear a range's values/formulas (styles kept). One undo group.",
            vec![
                ("range", prop("string", "A1-style range, e.g. \"A1:C10\".")),
                sheet(),
                target(),
            ],
            &["range"],
        ),
        tool(
            "xlsxy_find",
            "Search cell values and formula text (case-insensitive) across all sheets, or one sheet.",
            vec![
                ("query", prop("string", "Text to search for.")),
                sheet(),
                target(),
            ],
            &["query"],
        ),
        tool(
            "xlsxy_recalc",
            "Recalculate the whole workbook (and refresh pivots).",
            vec![target()],
            &[],
        ),
        tool(
            "xlsxy_save",
            "Save the open workbook to its file.",
            vec![target()],
            &[],
        ),
        tool(
            "xlsxy_comments",
            "List every cell comment (threads flattened in reply order): sheet, cell ref, author, text.",
            vec![target()],
            &[],
        ),
        tool(
            "xlsxy_comment_add",
            "Add a threaded comment to a cell (or a reply, if the cell already has a thread).",
            vec![
                ("ref", prop("string", "Cell reference, e.g. \"B4\".")),
                ("text", prop("string", "Comment text.")),
                (
                    "author",
                    prop(
                        "string",
                        "Comment author (defaults to the editing identity).",
                    ),
                ),
                sheet(),
                target(),
            ],
            &["ref", "text"],
        ),
        tool(
            "xlsxy_comment_remove",
            "Remove the comment (threaded or legacy note) on a cell, if any.",
            vec![
                ("ref", prop("string", "Cell reference, e.g. \"B4\".")),
                sheet(),
                target(),
            ],
            &["ref"],
        ),
        tool(
            "xlsxy_range_set",
            "Write a rectangular block of cells starting at a top-left ref, atomically: every \
             formula in the batch is validated before anything is applied. One undo group.",
            vec![
                (
                    "start",
                    prop("string", "Top-left cell reference, e.g. \"B4\"."),
                ),
                (
                    "rows",
                    prop_array(
                        item_array(item_ty("string")),
                        "Rows of cell text, each row an array of strings entered like xlsxy_set's \
                         text (empty string clears the cell).",
                    ),
                ),
                sheet(),
                target(),
            ],
            &["start", "rows"],
        ),
        tool(
            "xlsxy_export_csv",
            "Export a sheet's cells as display-formatted, RFC-4180 CSV.",
            vec![sheet(), target()],
            &[],
        ),
        tool(
            "xlsxy_import_csv",
            "Import CSV text as a brand-new sheet (never overwrites an existing one; name \
             collisions are deduplicated).",
            vec![
                ("text", prop("string", "CSV text to import.")),
                (
                    "name",
                    prop(
                        "string",
                        "Requested sheet name (default: \"Sheet\", deduplicated).",
                    ),
                ),
                target(),
            ],
            &["text"],
        ),
        tool(
            "xlsxy_pivot",
            "Compute an ad-hoc pivot table over a range (first row = header names); read-only, no \
             workbook mutation.",
            vec![
                (
                    "range",
                    prop(
                        "string",
                        "A1-style range, e.g. \"A1:D100\", first row = headers.",
                    ),
                ),
                (
                    "rows",
                    prop_array(
                        item_ty("string"),
                        "Header names to group by, as pivot rows.",
                    ),
                ),
                (
                    "cols",
                    prop_array(
                        item_ty("string"),
                        "Header names to group by, as pivot columns.",
                    ),
                ),
                (
                    "values",
                    prop_array(
                        item_obj(
                            vec![
                                (
                                    "col",
                                    prop("string", "Header name of the column to aggregate."),
                                ),
                                (
                                    "agg",
                                    prop(
                                        "string",
                                        "Aggregation: sum, count, countNums, average, max, min, \
                                         product, stdDev, stdDevP, var, or varP.",
                                    ),
                                ),
                            ],
                            &["col", "agg"],
                        ),
                        "Measures to compute, each a {col, agg} pair.",
                    ),
                ),
                sheet(),
                target(),
            ],
            &["range", "rows", "values"],
        ),
        tool(
            "xlsxy_replace_all",
            "Literal find/replace across every cell's input text, on every sheet. One undo group.",
            vec![
                ("query", prop("string", "Text to search for.")),
                ("text", prop("string", "Replacement text.")),
                target(),
            ],
            &["query", "text"],
        ),
        tool(
            "xlsxy_sheet_add",
            "Add a new sheet (deduplicated name on collision — never errors on a taken name).",
            vec![
                (
                    "name",
                    prop(
                        "string",
                        "Requested sheet name (default: \"Sheet\", deduplicated).",
                    ),
                ),
                target(),
            ],
            &[],
        ),
        tool(
            "xlsxy_sheet_remove",
            "Remove a sheet (errors on the last one — a workbook must keep at least one).",
            vec![
                ("sheet", prop("string", "Sheet index or name to remove.")),
                target(),
            ],
            &["sheet"],
        ),
        tool(
            "xlsxy_sheet_rename",
            "Rename a sheet and rewrite every formula/defined-name reference to it.",
            vec![
                ("sheet", prop("string", "Sheet index or name to rename.")),
                ("name", prop("string", "New sheet name.")),
                target(),
            ],
            &["sheet", "name"],
        ),
        tool(
            "xlsxy_row_insert",
            "Insert rows at a 0-based row index.",
            vec![
                ("at", prop("integer", "0-based row index to insert at.")),
                (
                    "count",
                    prop("integer", "Number of rows to insert (default 1)."),
                ),
                sheet(),
                target(),
            ],
            &["at"],
        ),
        tool(
            "xlsxy_row_delete",
            "Delete rows at a 0-based row index.",
            vec![
                ("at", prop("integer", "0-based row index to delete from.")),
                (
                    "count",
                    prop("integer", "Number of rows to delete (default 1)."),
                ),
                sheet(),
                target(),
            ],
            &["at"],
        ),
        tool(
            "xlsxy_col_insert",
            "Insert columns at a 0-based column index.",
            vec![
                ("at", prop("integer", "0-based column index to insert at.")),
                (
                    "count",
                    prop("integer", "Number of columns to insert (default 1)."),
                ),
                sheet(),
                target(),
            ],
            &["at"],
        ),
        tool(
            "xlsxy_col_delete",
            "Delete columns at a 0-based column index.",
            vec![
                (
                    "at",
                    prop("integer", "0-based column index to delete from."),
                ),
                (
                    "count",
                    prop("integer", "Number of columns to delete (default 1)."),
                ),
                sheet(),
                target(),
            ],
            &["at"],
        ),
        tool(
            "xlsxy_eval",
            "Side-effect-free formula preview: evaluate a formula against the live workbook at a \
             cell without writing anywhere.",
            vec![
                (
                    "formula",
                    prop(
                        "string",
                        "Formula to evaluate, e.g. \"SUM(B1:B3)\" (leading '=' optional).",
                    ),
                ),
                (
                    "ref",
                    prop("string", "Cell to evaluate at, e.g. \"B4\" (default A1)."),
                ),
                sheet(),
                target(),
            ],
            &["formula"],
        ),
        tool(
            "xlsxy_stats",
            "Summary statistics (sum, count, countNums, average, min, max) over a range.",
            vec![
                ("range", prop("string", "A1-style range, e.g. \"A1:C10\".")),
                sheet(),
                target(),
            ],
            &["range"],
        ),
        tool(
            "xlsxy_charts",
            "List every chart in the workbook: kind, title, categories, and series.",
            vec![target()],
            &[],
        ),
        tool(
            "xlsxy_pivots",
            "List every persistent pivot table: sheet, row/column fields, and value fields.",
            vec![target()],
            &[],
        ),
        tool(
            "xlsxy_format",
            "Apply cell formatting (number format, bold/italic, font/fill color, alignment) to \
             every cell in a range. One undo group. `xlsxy_get`'s reply echoes a cell's current \
             format the same way, for read-modify-write.",
            vec![
                ("range", prop("string", "A1-style range, e.g. \"A1:C10\".")),
                (
                    "patch",
                    prop_obj(
                        vec![
                            (
                                "numFmt",
                                prop(
                                    "string",
                                    "Number format code, e.g. \"0.00%\" or \"m/d/yyyy\".",
                                ),
                            ),
                            ("bold", prop("boolean", "Bold on/off.")),
                            ("italic", prop("boolean", "Italic on/off.")),
                            ("fontColor", prop("string", "Font color as \"#RRGGBB\".")),
                            (
                                "fillColor",
                                prop("string", "Fill (background) color as \"#RRGGBB\"."),
                            ),
                            (
                                "align",
                                prop(
                                    "string",
                                    "Horizontal alignment: \"left\", \"center\", or \"right\".",
                                ),
                            ),
                        ],
                        &[],
                        "Formatting to apply — at least one key required; an unknown key errors \
                         naming it. Keys absent from the patch leave that aspect of each cell's \
                         existing style untouched.",
                    ),
                ),
                sheet(),
                target(),
            ],
            &["range", "patch"],
        ),
        tool(
            "xlsxy_col_width",
            "Set one column's display width, in Excel column-width units.",
            vec![
                (
                    "col",
                    prop(
                        "string",
                        "Column letter (e.g. \"C\") or 0-based index; the reply echoes the \
                         numeric index.",
                    ),
                ),
                (
                    "width",
                    prop("number", "New column width; must be positive."),
                ),
                sheet(),
                target(),
            ],
            &["col", "width"],
        ),
        tool(
            "xlsxy_pivot_create",
            "Create a REAL, persistent pivot table on a new sheet (first row of `range` = header \
             names) — unlike `xlsxy_pivot`, this mutates the workbook: the pivot participates in \
             `xlsxy_pivots` and its output is refreshed by `xlsxy_recalc`. Clears undo history \
             like adding a sheet (an agent-level undo must remove both the created sheet and the \
             pivot registration).",
            vec![
                (
                    "range",
                    prop(
                        "string",
                        "A1-style range, e.g. \"A1:D100\", first row = headers.",
                    ),
                ),
                (
                    "rows",
                    prop_array(
                        item_ty("string"),
                        "Header names to group by, as pivot rows.",
                    ),
                ),
                (
                    "cols",
                    prop_array(
                        item_ty("string"),
                        "Header names to group by, as pivot columns.",
                    ),
                ),
                (
                    "values",
                    prop_array(
                        item_obj(
                            vec![
                                (
                                    "col",
                                    prop("string", "Header name of the column to aggregate."),
                                ),
                                (
                                    "agg",
                                    prop(
                                        "string",
                                        "Aggregation: sum, count, countNums, average, max, min, \
                                         product, stdDev, stdDevP, var, or varP.",
                                    ),
                                ),
                            ],
                            &["col", "agg"],
                        ),
                        "Measures to compute, each a {col, agg} pair.",
                    ),
                ),
                (
                    "name",
                    prop(
                        "string",
                        "Destination sheet name for the pivot output (default: PivotN).",
                    ),
                ),
                sheet(),
                target(),
            ],
            &["range", "rows", "values"],
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_xlsx_bytes_load_back_with_one_sheet() {
        let pkg = gridcore::xlsx::load_xlsx(&blank_xlsx_bytes()).expect("blank loads");
        // new_xlsx() ships exactly one sheet named Sheet1; assert via the workbook accessor.
        assert_eq!(pkg.workbook.sheets.len(), 1);
    }

    #[test]
    fn committed_blank_template_matches_blank_xlsx_bytes() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../offxy-vscode/mcp/templates/blank.xlsx");
        let bytes = std::fs::read(&p).expect("template committed");
        assert_eq!(
            bytes,
            blank_xlsx_bytes(),
            "regenerate the template (see plan Task 4)"
        );
    }

    #[test]
    fn tool_defs_include_xlsxy_new_with_required_path() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t.get_str("name")).collect();
        let list_pos = names.iter().position(|n| *n == "xlsxy_list").unwrap();
        assert_eq!(names[list_pos + 1], "xlsxy_new");
        let new_tool = tools
            .iter()
            .find(|t| t.get_str("name") == Some("xlsxy_new"))
            .unwrap();
        let req = new_tool
            .get("inputSchema")
            .unwrap()
            .get("required")
            .unwrap();
        assert_eq!(req.to_string(), "[\"path\"]");
    }

    #[test]
    fn tools_list_includes_the_grid_verbs() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t.get_str("name")).collect();
        for expected in [
            "xlsxy_list",
            "xlsxy_read",
            "xlsxy_set",
            "xlsxy_clear",
            "xlsxy_recalc",
            "xlsxy_save",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        for t in tools {
            assert_eq!(
                t.get("inputSchema").unwrap().get_str("type"),
                Some("object")
            );
        }
    }

    #[test]
    fn unknown_tool_is_reported() {
        let err = do_tool("xlsxy_nonesuch", &Json::obj(vec![])).unwrap_err();
        assert!(err.contains("unknown tool"));
    }

    #[test]
    fn wave1_tools_are_present_and_ordered_after_the_existing_ones() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t.get_str("name")).collect();
        let expected_tail = [
            "xlsxy_comments",
            "xlsxy_comment_add",
            "xlsxy_comment_remove",
            "xlsxy_range_set",
            "xlsxy_export_csv",
            "xlsxy_import_csv",
            "xlsxy_pivot",
            "xlsxy_replace_all",
            "xlsxy_sheet_add",
            "xlsxy_sheet_remove",
            "xlsxy_sheet_rename",
            "xlsxy_row_insert",
            "xlsxy_row_delete",
            "xlsxy_col_insert",
            "xlsxy_col_delete",
            "xlsxy_eval",
            "xlsxy_stats",
            "xlsxy_charts",
            "xlsxy_pivots",
            // Wave-2: appended last, same relative order everywhere.
            "xlsxy_format",
            "xlsxy_col_width",
            // Wave-3: appended last, same relative order everywhere.
            "xlsxy_pivot_create",
        ];
        let save_pos = names.iter().position(|n| *n == "xlsxy_save").unwrap();
        assert_eq!(
            &names[save_pos + 1..],
            &expected_tail,
            "wave-1/wave-2/wave-3 tools must be appended right after xlsxy_save, in this order"
        );
        for t in tools {
            assert_eq!(
                t.get("inputSchema").unwrap().get_str("type"),
                Some("object")
            );
        }
    }

    #[test]
    fn wave1_tool_required_arrays_match_the_spec() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let required_of = |name: &str| -> String {
            tools
                .iter()
                .find(|t| t.get_str("name") == Some(name))
                .unwrap_or_else(|| panic!("missing tool {name}"))
                .get("inputSchema")
                .unwrap()
                .get("required")
                .unwrap()
                .to_string()
        };
        assert_eq!(required_of("xlsxy_comments"), "[]");
        assert_eq!(required_of("xlsxy_comment_add"), "[\"ref\",\"text\"]");
        assert_eq!(required_of("xlsxy_comment_remove"), "[\"ref\"]");
        assert_eq!(required_of("xlsxy_range_set"), "[\"start\",\"rows\"]");
        assert_eq!(required_of("xlsxy_export_csv"), "[]");
        assert_eq!(required_of("xlsxy_import_csv"), "[\"text\"]");
        assert_eq!(
            required_of("xlsxy_pivot"),
            "[\"range\",\"rows\",\"values\"]"
        );
        assert_eq!(required_of("xlsxy_replace_all"), "[\"query\",\"text\"]");
        assert_eq!(required_of("xlsxy_sheet_add"), "[]");
        assert_eq!(required_of("xlsxy_sheet_remove"), "[\"sheet\"]");
        assert_eq!(required_of("xlsxy_sheet_rename"), "[\"sheet\",\"name\"]");
        assert_eq!(required_of("xlsxy_row_insert"), "[\"at\"]");
        assert_eq!(required_of("xlsxy_row_delete"), "[\"at\"]");
        assert_eq!(required_of("xlsxy_col_insert"), "[\"at\"]");
        assert_eq!(required_of("xlsxy_col_delete"), "[\"at\"]");
        assert_eq!(required_of("xlsxy_eval"), "[\"formula\"]");
        assert_eq!(required_of("xlsxy_stats"), "[\"range\"]");
        assert_eq!(required_of("xlsxy_charts"), "[]");
        assert_eq!(required_of("xlsxy_pivots"), "[]");
        assert_eq!(required_of("xlsxy_format"), "[\"range\",\"patch\"]");
        assert_eq!(required_of("xlsxy_col_width"), "[\"col\",\"width\"]");
        // Matches xlsxy_pivot's actual required set exactly (cols/name/sheet optional).
        assert_eq!(
            required_of("xlsxy_pivot_create"),
            "[\"range\",\"rows\",\"values\"]"
        );
    }

    #[test]
    fn wave1_array_arg_schemas_have_the_expected_nested_shape() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let props_of = |name: &str| -> Json {
            tools
                .iter()
                .find(|t| t.get_str("name") == Some(name))
                .unwrap()
                .get("inputSchema")
                .unwrap()
                .get("properties")
                .unwrap()
                .clone()
        };

        // xlsxy_range_set.rows: array of arrays of strings.
        let rows_schema = props_of("xlsxy_range_set").get("rows").unwrap().clone();
        assert_eq!(rows_schema.get_str("type"), Some("array"));
        let rows_items = rows_schema.get("items").unwrap();
        assert_eq!(rows_items.get_str("type"), Some("array"));
        assert_eq!(
            rows_items.get("items").unwrap().get_str("type"),
            Some("string")
        );

        // xlsxy_pivot.rows / .cols: arrays of strings.
        let pivot_props = props_of("xlsxy_pivot");
        for key in ["rows", "cols"] {
            let schema = pivot_props.get(key).unwrap();
            assert_eq!(schema.get_str("type"), Some("array"));
            assert_eq!(schema.get("items").unwrap().get_str("type"), Some("string"));
        }

        // xlsxy_pivot.values: array of {col, agg} objects, both required.
        let values_schema = pivot_props.get("values").unwrap();
        assert_eq!(values_schema.get_str("type"), Some("array"));
        let item = values_schema.get("items").unwrap();
        assert_eq!(item.get_str("type"), Some("object"));
        assert!(item.get("properties").unwrap().get("col").is_some());
        assert!(item.get("properties").unwrap().get("agg").is_some());
        assert_eq!(
            item.get("required").unwrap().to_string(),
            "[\"col\",\"agg\"]"
        );
    }

    /// Wave-3: `xlsxy_pivot_create`'s arg shape mirrors `xlsxy_pivot`'s
    /// (range/rows/cols/values with the same nested shapes) plus an
    /// additional optional `name` string.
    #[test]
    fn xlsxy_pivot_create_mirrors_xlsxy_pivot_plus_name() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let props_of = |name: &str| -> Json {
            tools
                .iter()
                .find(|t| t.get_str("name") == Some(name))
                .unwrap()
                .get("inputSchema")
                .unwrap()
                .get("properties")
                .unwrap()
                .clone()
        };
        let pivot_props = props_of("xlsxy_pivot");
        let create_props = props_of("xlsxy_pivot_create");
        for key in ["range", "rows", "cols", "values", "sheet", "target"] {
            assert_eq!(
                pivot_props.get(key).unwrap().to_string(),
                create_props.get(key).unwrap().to_string(),
                "xlsxy_pivot_create.{key} must mirror xlsxy_pivot.{key} exactly"
            );
        }
        let name_prop = create_props
            .get("name")
            .expect("xlsxy_pivot_create missing 'name' prop");
        assert_eq!(name_prop.get_str("type"), Some("string"));
        assert_eq!(
            name_prop.get_str("description"),
            Some("Destination sheet name for the pivot output (default: PivotN).")
        );
    }

    /// Wave-2: `xlsxy_format.patch` is an object schema with the six
    /// optional typed properties (all described), and no required keys of
    /// its own (the tool-level `required` covers `range`/`patch`).
    #[test]
    fn xlsxy_format_patch_schema_has_the_six_optional_typed_properties() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let patch_schema = tools
            .iter()
            .find(|t| t.get_str("name") == Some("xlsxy_format"))
            .unwrap()
            .get("inputSchema")
            .unwrap()
            .get("properties")
            .unwrap()
            .get("patch")
            .unwrap()
            .clone();
        assert_eq!(patch_schema.get_str("type"), Some("object"));
        assert!(patch_schema.get_str("description").is_some());
        assert_eq!(patch_schema.get("required").unwrap().to_string(), "[]");
        let props = patch_schema.get("properties").unwrap();
        let expected_types = [
            ("numFmt", "string"),
            ("bold", "boolean"),
            ("italic", "boolean"),
            ("fontColor", "string"),
            ("fillColor", "string"),
            ("align", "string"),
        ];
        for (key, ty) in expected_types {
            let p = props
                .get(key)
                .unwrap_or_else(|| panic!("patch missing key {key}"));
            assert_eq!(p.get_str("type"), Some(ty), "wrong type for patch.{key}");
            assert!(
                p.get_str("description").is_some(),
                "patch.{key} missing description"
            );
        }
    }

    /// `xlsxy_col_width.col` documents that it accepts a letter or 0-based
    /// index, and that the reply echoes the numeric index — matching
    /// `col.width`'s actual reply shape (`{col: <number>, width}`).
    #[test]
    fn xlsxy_col_width_col_description_notes_numeric_echo() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let props = tools
            .iter()
            .find(|t| t.get_str("name") == Some("xlsxy_col_width"))
            .unwrap()
            .get("inputSchema")
            .unwrap()
            .get("properties")
            .unwrap()
            .clone();
        let col_desc = props.get("col").unwrap().get_str("description").unwrap();
        assert!(col_desc.contains("index"), "{col_desc}");
        assert!(
            col_desc.to_lowercase().contains("reply echoes"),
            "{col_desc}"
        );
        assert_eq!(props.get("width").unwrap().get_str("type"), Some("number"));
    }

    /// Every forwarding tool → its exact spec verb string, pre-existing tools
    /// included (cheap, and it pins the whole surface, not just wave-1).
    const VERB_TABLE: &[(&str, &str)] = &[
        ("xlsxy_status", "wb.path"),
        ("xlsxy_sheets", "sheet.list"),
        ("xlsxy_read", "sheet.read"),
        ("xlsxy_get", "cell.get"),
        ("xlsxy_set", "cell.set"),
        ("xlsxy_clear", "range.clear"),
        ("xlsxy_find", "find"),
        ("xlsxy_recalc", "wb.recalc"),
        ("xlsxy_save", "wb.save"),
        ("xlsxy_comments", "comment.list"),
        ("xlsxy_comment_add", "comment.add"),
        ("xlsxy_comment_remove", "comment.remove"),
        ("xlsxy_range_set", "range.set"),
        ("xlsxy_export_csv", "wb.export-csv"),
        ("xlsxy_import_csv", "sheet.import-csv"),
        ("xlsxy_pivot", "sheet.pivot"),
        ("xlsxy_replace_all", "wb.replace-all"),
        ("xlsxy_sheet_add", "sheet.add"),
        ("xlsxy_sheet_remove", "sheet.remove"),
        ("xlsxy_sheet_rename", "sheet.rename"),
        ("xlsxy_row_insert", "row.insert"),
        ("xlsxy_row_delete", "row.delete"),
        ("xlsxy_col_insert", "col.insert"),
        ("xlsxy_col_delete", "col.delete"),
        ("xlsxy_eval", "formula.eval"),
        ("xlsxy_stats", "sheet.stats"),
        ("xlsxy_charts", "chart.list"),
        ("xlsxy_pivots", "pivot.list"),
        ("xlsxy_format", "cell.format"),
        ("xlsxy_col_width", "col.width"),
        ("xlsxy_pivot_create", "pivot.create"),
    ];
    /// Tools handled specially in `do_tool` (not simple verb forwards), so
    /// `verb_for` deliberately returns `None` for them.
    const SPECIALLY_HANDLED: &[&str] = &["xlsxy_list", "xlsxy_new"];

    #[test]
    fn verb_for_maps_every_tool_to_its_exact_spec_verb() {
        // A swapped-but-valid mapping (e.g. xlsxy_row_insert -> row.delete)
        // must fail this test, not just "resolves to something" — that's the
        // whole point of pinning the exact string per tool.
        for (name, verb) in VERB_TABLE {
            assert_eq!(verb_for(name), Some(*verb), "wrong verb for {name}");
        }
        for name in SPECIALLY_HANDLED {
            assert_eq!(
                verb_for(name),
                None,
                "{name} is handled specially in do_tool, verb_for must return None"
            );
        }
        // Every tool_defs() name must appear in exactly one of the two lists
        // above — catches a newly added tool whose verb_for entry (or
        // special-case) was forgotten.
        let defs = tool_defs();
        let all_names: Vec<&str> = defs
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get_str("name"))
            .collect();
        assert_eq!(
            all_names.len(),
            VERB_TABLE.len() + SPECIALLY_HANDLED.len(),
            "VERB_TABLE + SPECIALLY_HANDLED must cover every tool exactly once"
        );
        for name in &all_names {
            let in_table = VERB_TABLE.iter().any(|(n, _)| n == name);
            let in_special = SPECIALLY_HANDLED.contains(name);
            assert!(
                in_table ^ in_special,
                "{name} must be in exactly one of VERB_TABLE/SPECIALLY_HANDLED"
            );
        }
    }
}
