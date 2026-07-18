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

/// Map a forwarding tool name to its exact ctl verb string — the single
/// source of truth `do_tool` dispatches through, so a test can pin every
/// tool's verb precisely (not just "resolves to *something*", which a
/// swapped-but-valid mapping would still pass). Returns `None` for
/// `docxy_list`/`docxy_new` (handled specially in `do_tool`, not simple
/// forwards) and for any unrecognized name.
pub(crate) fn verb_for(name: &str) -> Option<&'static str> {
    Some(match name {
        "docxy_status" => "doc.path",
        "docxy_outline" => "doc.outline",
        "docxy_read" => "doc.read",
        "docxy_find" => "doc.find",
        "docxy_replace_range" => "doc.replace-range",
        "docxy_insert" => "doc.insert",
        "docxy_append" => "doc.append",
        "docxy_save" => "doc.save",
        "docxy_export" => "doc.export",
        "docxy_export_pdf" => "doc.export-pdf",
        "docxy_comments" => "doc.comments",
        "docxy_notes" => "doc.notes",
        "docxy_header" => "doc.header",
        "docxy_footer" => "doc.footer",
        "docxy_metadata" => "doc.metadata",
        "docxy_stats" => "doc.stats",
        "docxy_replace_all" => "doc.replace-all",
        "docxy_undo" => "doc.undo",
        "docxy_redo" => "doc.redo",
        _ => return None,
    })
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
    let verb = verb_for(name).ok_or_else(|| format!("unknown tool: {name}"))?;
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
        tool(
            "docxy_export",
            "Export the live document (including unsaved edits) as Markdown or plain text.",
            vec![
                (
                    "format",
                    prop("string", "Output format: \"markdown\" or \"text\"."),
                ),
                target(),
            ],
            &["format"],
        ),
        tool(
            "docxy_export_pdf",
            "Render the live document to a PDF at a path. Refuses to overwrite an existing file.",
            vec![
                (
                    "path",
                    prop("string", "File path for the PDF (created; must not exist)."),
                ),
                target(),
            ],
            &["path"],
        ),
        tool(
            "docxy_comments",
            "List the document's review comments (author, initials, date, text, anchor), in anchor order.",
            vec![target()],
            &[],
        ),
        tool(
            "docxy_notes",
            "List the document's footnotes and endnotes, in file order.",
            vec![target()],
            &[],
        ),
        tool(
            "docxy_header",
            "Read the default section header's block content (empty if the document has none).",
            vec![target()],
            &[],
        ),
        tool(
            "docxy_footer",
            "Read the default section footer's block content (empty if the document has none).",
            vec![target()],
            &[],
        ),
        tool(
            "docxy_metadata",
            "Read the document's core properties (title, author, subject, keywords, comments, \
             last saved by, revision, created, modified) — present-if-set.",
            vec![target()],
            &[],
        ),
        tool(
            "docxy_stats",
            "Word/character/paragraph/block counts over the live document.",
            vec![target()],
            &[],
        ),
        tool(
            "docxy_replace_all",
            "Replace every occurrence of a query with text across the whole document \
             (case-insensitive unless case_sensitive:true). Undoable.",
            vec![
                ("query", prop("string", "Text to search for.")),
                ("text", prop("string", "Replacement text.")),
                (
                    "case_sensitive",
                    prop("boolean", "Match case (default false)."),
                ),
                target(),
            ],
            &["query", "text"],
        ),
        tool(
            "docxy_undo",
            "Undo the last edit, if any. Returns {done:false} when there is nothing to undo.",
            vec![target()],
            &[],
        ),
        tool(
            "docxy_redo",
            "Redo the last undone edit, if any. Returns {done:false} when there is nothing to redo.",
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
    fn committed_blank_template_matches_blank_docx_bytes() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../offxy-vscode/mcp/templates/blank.docx");
        let bytes = std::fs::read(&p).expect("template committed");
        assert_eq!(
            bytes,
            blank_docx_bytes(),
            "regenerate the template (see plan Task 4)"
        );
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
    fn wave1_tools_are_present_and_ordered_after_the_existing_ones() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t.get_str("name")).collect();
        let expected_tail = [
            "docxy_export",
            "docxy_export_pdf",
            "docxy_comments",
            "docxy_notes",
            "docxy_header",
            "docxy_footer",
            "docxy_metadata",
            "docxy_stats",
            "docxy_replace_all",
            "docxy_undo",
            "docxy_redo",
        ];
        let save_pos = names.iter().position(|n| *n == "docxy_save").unwrap();
        assert_eq!(
            &names[save_pos + 1..],
            &expected_tail,
            "wave-1 tools must be appended right after docxy_save, in this order"
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
        assert_eq!(required_of("docxy_export"), "[\"format\"]");
        assert_eq!(required_of("docxy_export_pdf"), "[\"path\"]");
        assert_eq!(required_of("docxy_comments"), "[]");
        assert_eq!(required_of("docxy_notes"), "[]");
        assert_eq!(required_of("docxy_header"), "[]");
        assert_eq!(required_of("docxy_footer"), "[]");
        assert_eq!(required_of("docxy_metadata"), "[]");
        assert_eq!(required_of("docxy_stats"), "[]");
        assert_eq!(required_of("docxy_replace_all"), "[\"query\",\"text\"]");
        assert_eq!(required_of("docxy_undo"), "[]");
        assert_eq!(required_of("docxy_redo"), "[]");
    }

    /// Every forwarding tool → its exact spec verb string, pre-existing tools
    /// included (cheap, and it pins the whole surface, not just wave-1).
    const VERB_TABLE: &[(&str, &str)] = &[
        ("docxy_status", "doc.path"),
        ("docxy_outline", "doc.outline"),
        ("docxy_read", "doc.read"),
        ("docxy_find", "doc.find"),
        ("docxy_replace_range", "doc.replace-range"),
        ("docxy_insert", "doc.insert"),
        ("docxy_append", "doc.append"),
        ("docxy_save", "doc.save"),
        ("docxy_export", "doc.export"),
        ("docxy_export_pdf", "doc.export-pdf"),
        ("docxy_comments", "doc.comments"),
        ("docxy_notes", "doc.notes"),
        ("docxy_header", "doc.header"),
        ("docxy_footer", "doc.footer"),
        ("docxy_metadata", "doc.metadata"),
        ("docxy_stats", "doc.stats"),
        ("docxy_replace_all", "doc.replace-all"),
        ("docxy_undo", "doc.undo"),
        ("docxy_redo", "doc.redo"),
    ];
    /// Tools handled specially in `do_tool` (not simple verb forwards), so
    /// `verb_for` deliberately returns `None` for them.
    const SPECIALLY_HANDLED: &[&str] = &["docxy_list", "docxy_new"];

    #[test]
    fn verb_for_maps_every_tool_to_its_exact_spec_verb() {
        // A swapped-but-valid mapping (e.g. docxy_undo -> doc.redo) must fail
        // this test, not just "resolves to something" — that's the whole
        // point of pinning the exact string per tool.
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

    #[test]
    fn list_running_shape_is_stable() {
        // With no docxy running (or no ctl dir), the list is present and empty-ish.
        let v = do_tool("docxy_list", &Json::obj(vec![])).unwrap();
        assert!(v.contains("\"running\":["));
    }
}
