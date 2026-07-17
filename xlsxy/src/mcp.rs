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
use ctlcore::mcp::{McpServer, prop, tool};

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

/// Execute a tool by forwarding to the control surface.
fn do_tool(name: &str, args: &Json) -> Result<String, String> {
    let dir = ctlcore::config_ctl_dir("xlsxy").ok_or("no control directory on this system")?;
    if name == "xlsxy_list" {
        return Ok(client::list_running(&dir, "xlsxy").to_string());
    }
    let verb = match name {
        "xlsxy_status" => "wb.path",
        "xlsxy_sheets" => "sheet.list",
        "xlsxy_read" => "sheet.read",
        "xlsxy_get" => "cell.get",
        "xlsxy_set" => "cell.set",
        "xlsxy_clear" => "range.clear",
        "xlsxy_find" => "find",
        "xlsxy_recalc" => "wb.recalc",
        "xlsxy_save" => "wb.save",
        other => return Err(format!("unknown tool: {other}")),
    };
    let client = client::resolve_target(&dir, "xlsxy", args.get_str("target"))?;
    let result = client.call(verb, args.clone())?;
    Ok(result.to_string())
}

const TARGET_DESC: &str =
    "Optional: which xlsxy to act on (a substring of its instance/pane id) when several are open.";
const SHEET_DESC: &str = "Optional sheet index or name (default: the active sheet).";

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
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
