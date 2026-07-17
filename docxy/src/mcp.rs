//! A [Model Context Protocol](https://modelcontextprotocol.io) stdio server that
//! exposes docxy's control verbs as native tools for an MCP client such as
//! Claude Code (`claude mcp add docxy -- docxy --mcp`).
//!
//! It is a thin adapter: a *client* of a running docxy's control surface (via
//! [`ctlcore::client`]), discovered through the ctl directory. The MCP process
//! opens no document of its own — it finds the docxy the user already has open
//! (e.g. in a sibling agwinterm pane) and forwards tool calls to it, so edits
//! land on that editor's live buffer and undo stack.
//!
//! Transport is newline-delimited JSON-RPC 2.0 over stdio: one message per line,
//! no embedded newlines, per the MCP stdio transport.

use crate::control;
use ctlcore::client::{self, Client};
use ctlcore::json::Json;
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Serve MCP over stdio until stdin closes.
pub fn run() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = Json::parse(trimmed) else {
            continue; // ignore anything that isn't a JSON message
        };
        if let Some(resp) = handle(&msg) {
            let mut s = resp.to_string();
            s.push('\n');
            out.write_all(s.as_bytes())?;
            out.flush()?;
        }
    }
    Ok(())
}

/// Route one JSON-RPC message. Returns `Some(response)` for requests, `None` for
/// notifications (and messages without a method).
fn handle(msg: &Json) -> Option<Json> {
    let method = msg.get_str("method")?;
    let id = msg.get("id").cloned().unwrap_or(Json::Null);
    match method {
        "initialize" => Some(ok(id, initialize_result())),
        "ping" => Some(ok(id, Json::obj(vec![]))),
        "tools/list" => Some(ok(id, Json::obj(vec![("tools", tool_defs())]))),
        "tools/call" => Some(handle_tool_call(id, msg.get("params"))),
        // Notifications (initialized, cancelled, …) expect no response.
        m if m.starts_with("notifications/") => None,
        other => Some(err(id, -32601, format!("method not found: {other}"))),
    }
}

fn handle_tool_call(id: Json, params: Option<&Json>) -> Json {
    let Some(params) = params else {
        return err(id, -32602, "missing params".into());
    };
    let Some(name) = params.get_str("name") else {
        return err(id, -32602, "missing tool name".into());
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Json::obj(vec![]));
    match do_tool(name, &args) {
        Ok(text) => ok(id, tool_result(text, false)),
        // A tool-level failure is a normal result with isError, not a protocol error.
        Err(e) => ok(id, tool_result(e, true)),
    }
}

/// Execute a tool by forwarding to the control surface. Returns the result text
/// (JSON) or an error message.
fn do_tool(name: &str, args: &Json) -> Result<String, String> {
    if name == "docxy_list" {
        return Ok(list_running().to_string());
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
    let client = resolve(args)?;
    // Control verbs ignore unknown keys, so forwarding `arguments` verbatim
    // (including any `target`) is harmless.
    let result = client.call(verb, args.clone())?;
    Ok(result.to_string())
}

/// The running docxy instances, as a tool result.
fn list_running() -> Json {
    let running = control::control_dir()
        .map(|dir| client::discover_live(&dir))
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.instance.starts_with("docxy-"))
        .map(|i| {
            Json::obj(vec![
                ("instance", Json::Str(i.instance)),
                ("port", Json::Num(i.port as f64)),
                ("pid", Json::Num(i.pid as f64)),
            ])
        })
        .collect();
    Json::obj(vec![("running", Json::Arr(running))])
}

/// Find the docxy to act on: the single running instance, or the one selected by
/// a `target` substring of its instance/pane id.
fn resolve(args: &Json) -> Result<Client, String> {
    let dir = control::control_dir().ok_or("no control directory on this system")?;
    let mut live: Vec<_> = client::discover_live(&dir)
        .into_iter()
        .filter(|i| i.instance.starts_with("docxy-"))
        .collect();
    if let Some(target) = args.get_str("target") {
        live.retain(|i| i.instance.contains(target));
    }
    match live.len() {
        0 => Err("no running docxy found — open a document in a docxy pane first".into()),
        1 => Ok(live.remove(0).client()),
        _ => {
            let names: Vec<&str> = live.iter().map(|i| i.instance.as_str()).collect();
            Err(format!(
                "several docxy instances are running ({}); pass \"target\" with a distinguishing substring (e.g. the pane id)",
                names.join(", ")
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC + MCP envelope helpers
// ---------------------------------------------------------------------------

fn ok(id: Json, result: Json) -> Json {
    Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".into())),
        ("id", id),
        ("result", result),
    ])
}

fn err(id: Json, code: i64, message: String) -> Json {
    Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".into())),
        ("id", id),
        (
            "error",
            Json::obj(vec![
                ("code", Json::Num(code as f64)),
                ("message", Json::Str(message)),
            ]),
        ),
    ])
}

fn tool_result(text: String, is_error: bool) -> Json {
    Json::obj(vec![
        (
            "content",
            Json::Arr(vec![Json::obj(vec![
                ("type", Json::Str("text".into())),
                ("text", Json::Str(text)),
            ])]),
        ),
        ("isError", Json::Bool(is_error)),
    ])
}

fn initialize_result() -> Json {
    Json::obj(vec![
        ("protocolVersion", Json::Str(PROTOCOL_VERSION.into())),
        (
            "capabilities",
            Json::obj(vec![("tools", Json::obj(vec![]))]),
        ),
        (
            "serverInfo",
            Json::obj(vec![
                ("name", Json::Str("docxy".into())),
                ("version", Json::Str(env!("CARGO_PKG_VERSION").into())),
            ]),
        ),
    ])
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

fn prop(ty: &str, desc: &str) -> Json {
    Json::obj(vec![
        ("type", Json::Str(ty.into())),
        ("description", Json::Str(desc.into())),
    ])
}

const TARGET_DESC: &str =
    "Optional: which docxy to act on (a substring of its instance/pane id) when several are open.";

fn tool(name: &str, description: &str, props: Vec<(&str, Json)>, required: &[&str]) -> Json {
    let schema = Json::obj(vec![
        ("type", Json::Str("object".into())),
        ("properties", Json::obj(props)),
        (
            "required",
            Json::Arr(required.iter().map(|s| Json::Str((*s).into())).collect()),
        ),
    ]);
    Json::obj(vec![
        ("name", Json::Str(name.into())),
        ("description", Json::Str(description.into())),
        ("inputSchema", schema),
    ])
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

    fn req(method: &str, id: i64) -> Json {
        Json::obj(vec![
            ("jsonrpc", Json::Str("2.0".into())),
            ("method", Json::Str(method.into())),
            ("id", Json::Num(id as f64)),
        ])
    }

    #[test]
    fn initialize_advertises_tools_capability() {
        let r = handle(&req("initialize", 1)).unwrap();
        let result = r.get("result").unwrap();
        assert_eq!(result.get_str("protocolVersion"), Some(PROTOCOL_VERSION));
        assert!(result.get("capabilities").unwrap().get("tools").is_some());
        assert_eq!(
            result.get("serverInfo").unwrap().get_str("name"),
            Some("docxy")
        );
        assert_eq!(r.get("id").unwrap().as_i64(), Some(1));
    }

    #[test]
    fn tools_list_includes_the_edit_verbs() {
        let r = handle(&req("tools/list", 2)).unwrap();
        let tools = r
            .get("result")
            .unwrap()
            .get("tools")
            .unwrap()
            .as_array()
            .unwrap();
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
    fn ping_returns_empty_result() {
        let r = handle(&req("ping", 3)).unwrap();
        assert!(r.get("result").is_some());
    }

    #[test]
    fn notifications_get_no_response() {
        let note = Json::obj(vec![
            ("jsonrpc", Json::Str("2.0".into())),
            ("method", Json::Str("notifications/initialized".into())),
        ]);
        assert!(handle(&note).is_none());
    }

    #[test]
    fn unknown_method_is_a_jsonrpc_error() {
        let r = handle(&req("frobnicate", 4)).unwrap();
        assert_eq!(
            r.get("error").unwrap().get("code").unwrap().as_i64(),
            Some(-32601)
        );
    }

    #[test]
    fn unknown_tool_reports_iserror_result() {
        let params = Json::obj(vec![
            ("name", Json::Str("docxy_nonesuch".into())),
            ("arguments", Json::obj(vec![])),
        ]);
        let r = handle_tool_call(Json::Num(5.0), Some(&params));
        let result = r.get("result").unwrap();
        assert_eq!(result.get("isError").unwrap().as_bool(), Some(true));
        let text = result.get("content").unwrap().as_array().unwrap()[0]
            .get_str("text")
            .unwrap();
        assert!(text.contains("unknown tool"));
    }

    #[test]
    fn list_running_shape_is_stable() {
        // With no docxy running (or no ctl dir), the list is present and empty-ish.
        let v = list_running();
        assert!(v.get("running").unwrap().as_array().is_some());
    }
}
