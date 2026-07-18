//! docxy's [MCP](https://modelcontextprotocol.io) stdio server: exposes the
//! control verbs as native tools for an MCP client such as Claude Code
//! (`claude mcp add docxy -- docxy --mcp`).
//!
//! It is a thin adapter: a *client* of a running docxy's control surface (via
//! [`ctlcore::client`]), discovered through the ctl directory. The MCP process
//! opens no document of its own — it finds the docxy the user already has open
//! (e.g. in a sibling agwinterm pane) and forwards tool calls to it, so edits
//! land on that editor's live buffer and undo stack. The protocol scaffolding
//! lives in [`ctlcore::mcp`].

use crate::control;
use ctlcore::client;
use ctlcore::json::Json;
use ctlcore::mcp::{McpServer, prop, tool};

/// Serve MCP over stdio until stdin closes.
pub fn run() -> std::io::Result<()> {
    McpServer {
        name: "docxy",
        version: env!("CARGO_PKG_VERSION"),
        tools: tool_defs(),
        handler: &do_tool,
    }
    .run()
}

/// Execute a tool by forwarding to the control surface. Returns the result text
/// (JSON) or an error message.
fn do_tool(name: &str, args: &Json) -> Result<String, String> {
    let dir = control::control_dir().ok_or("no control directory on this system")?;
    if name == "docxy_list" {
        return Ok(client::list_running(&dir, "docxy").to_string());
    }
    if name == "docxy_new" {
        return Ok(
            client::new_file(&dir, "docxy", "doc.open", &blank_docx_bytes(), args)?.to_string(),
        );
    }
    let verb = match name {
        "docxy_status" => "doc.path",
        "docxy_outline" => "doc.outline",
        "docxy_read" => "doc.read",
        "docxy_find" => "doc.find",
        "docxy_replace_range" => "doc.replace-range",
        "docxy_insert" => "doc.insert",
        "docxy_append" => "doc.append",
        "docxy_save" => "doc.save",
        other => return Err(format!("unknown tool: {other}")),
    };
    let client = client::resolve_target(&dir, "docxy", args.get_str("target"))?;
    // Control verbs ignore unknown keys, so forwarding `arguments` verbatim
    // (including any `target`) is harmless.
    let result = client.call(verb, args.clone())?;
    Ok(result.to_string())
}

const TARGET_DESC: &str =
    "Optional: which docxy to act on (a substring of its instance/pane id) when several are open.";

/// A minimal valid .docx: one empty paragraph in a fresh OPC package. Also the
/// source of the committed template the bundled VS Code MCP server ships.
pub(crate) fn blank_docx_bytes() -> Vec<u8> {
    use docxcore::model::{Block, Document, Inline, ParProps, Paragraph, Run, RunProps};
    let doc = Document {
        body: vec![Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: vec![Inline::Run(Run {
                text: String::new(),
                props: RunProps::default(),
            })],
        })],
    };
    docxcore::package::save_package(&docxcore::package::new_package(doc))
}

fn tool_defs() -> Json {
    let target = || ("target", prop("string", TARGET_DESC));
    Json::Arr(vec![
        tool(
            "docxy_list",
            "List the docxy editors currently running on this machine (instance/pane id, port, pid).",
            vec![],
            &[],
        ),
        tool(
            "docxy_new",
            "Create a new blank .docx at a path and open it in the running docxy (in a VS Code \
             window, a new tab). With no docxy running the file is still created. Refuses to \
             overwrite an existing file.",
            vec![
                (
                    "path",
                    prop(
                        "string",
                        "File path for the new document (created; must not exist).",
                    ),
                ),
                target(),
            ],
            &["path"],
        ),
        tool(
            "docxy_status",
            "Report the open document's path, format, modified flag, and block count.",
            vec![target()],
            &[],
        ),
        tool(
            "docxy_outline",
            "Return the document's heading outline: each heading's block index, level, and text.",
            vec![target()],
            &[],
        ),
        tool(
            "docxy_read",
            "Read the live document (including unsaved edits). Returns per-block text + kind; \
             defaults to the whole document, or pass a block range.",
            vec![
                ("start", prop("integer", "First block index (default 0).")),
                (
                    "end",
                    prop("integer", "Last block index, inclusive (default: last)."),
                ),
                target(),
            ],
            &[],
        ),
        tool(
            "docxy_find",
            "Find all occurrences of a query in the live document; returns match positions and the containing paragraph.",
            vec![
                ("query", prop("string", "Text to search for.")),
                (
                    "case_sensitive",
                    prop("boolean", "Match case (default false)."),
                ),
                target(),
            ],
            &["query"],
        ),
        tool(
            "docxy_replace_range",
            "Replace paragraphs [start..=end] with new text (\\n separates paragraphs). Undoable; \
             endpoints must be paragraphs.",
            vec![
                (
                    "start",
                    prop("integer", "First paragraph block index to replace."),
                ),
                (
                    "end",
                    prop(
                        "integer",
                        "Last paragraph block index, inclusive (default: start).",
                    ),
                ),
                (
                    "text",
                    prop("string", "Replacement text; \\n starts a new paragraph."),
                ),
                target(),
            ],
            &["start", "text"],
        ),
        tool(
            "docxy_insert",
            "Insert text as new paragraph(s) before the block at `at` (\\n separates paragraphs). Undoable.",
            vec![
                (
                    "at",
                    prop(
                        "integer",
                        "Block index to insert before (== block count to append).",
                    ),
                ),
                (
                    "text",
                    prop("string", "Text to insert; \\n starts a new paragraph."),
                ),
                target(),
            ],
            &["at", "text"],
        ),
        tool(
            "docxy_append",
            "Append text as new paragraph(s) at the end of the document (\\n separates paragraphs). Undoable.",
            vec![
                (
                    "text",
                    prop("string", "Text to append; \\n starts a new paragraph."),
                ),
                target(),
            ],
            &["text"],
        ),
        tool(
            "docxy_save",
            "Save the open document to its file.",
            vec![target()],
            &[],
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_docx_bytes_load_back_as_one_empty_paragraph() {
        let pkg = docxcore::package::load_package(&blank_docx_bytes()).expect("blank loads");
        assert_eq!(pkg.document.body.len(), 1);
        assert_eq!(pkg.document.plain_text(), "\n");
    }

    #[test]
    fn tool_defs_include_docxy_new_with_required_path() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        // Ordered right after docxy_list (parity with the bundled server).
        let names: Vec<&str> = tools.iter().filter_map(|t| t.get_str("name")).collect();
        let list_pos = names.iter().position(|n| *n == "docxy_list").unwrap();
        assert_eq!(names[list_pos + 1], "docxy_new");
        let new_tool = tools
            .iter()
            .find(|t| t.get_str("name") == Some("docxy_new"))
            .unwrap();
        let req = new_tool
            .get("inputSchema")
            .unwrap()
            .get("required")
            .unwrap();
        assert_eq!(req.to_string(), "[\"path\"]");
    }

    #[test]
    fn tools_list_includes_the_edit_verbs() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t.get_str("name")).collect();
        for expected in [
            "docxy_list",
            "docxy_read",
            "docxy_replace_range",
            "docxy_insert",
            "docxy_append",
            "docxy_save",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        // Every tool carries an object input schema.
        for t in tools {
            assert_eq!(
                t.get("inputSchema").unwrap().get_str("type"),
                Some("object")
            );
        }
    }

    #[test]
    fn unknown_tool_is_reported() {
        let err = do_tool("docxy_nonesuch", &Json::obj(vec![])).unwrap_err();
        assert!(err.contains("unknown tool"));
    }

    #[test]
    fn list_running_shape_is_stable() {
        // With no docxy running (or no ctl dir), the list is present and empty-ish.
        let v = do_tool("docxy_list", &Json::obj(vec![])).unwrap();
        assert!(v.contains("\"running\":["));
    }
}
