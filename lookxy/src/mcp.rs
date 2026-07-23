//! lookxy's [MCP](https://modelcontextprotocol.io) stdio server: exposes the
//! mail control verbs as native tools for an MCP client such as Claude Code
//! (`claude mcp add lookxy -- lookxy --mcp`).
//!
//! It is a thin adapter: a *client* of a running lookxy's control surface (via
//! [`ctlcore::client`]), discovered through the ctl directory. The MCP
//! process opens no mailbox of its own — it finds the lookxy the user already
//! has open (e.g. in a sibling agwinterm pane) and forwards tool calls to it,
//! so triage lands on that instance's live store and outbox. The protocol
//! scaffolding lives in [`ctlcore::mcp`].

use crate::control;
use ctlcore::client;
use ctlcore::json::Json;
use ctlcore::mcp::{McpServer, prop, tool};

/// Serve MCP over stdio until stdin closes.
pub fn run() -> std::io::Result<()> {
    McpServer {
        name: "lookxy",
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
    if name == "lookxy_list" {
        return Ok(client::list_running(&dir, "lookxy").to_string());
    }
    let verb = match name {
        "lookxy_status" => "mail.status",
        "lookxy_folders" => "mail.folders",
        "lookxy_messages" => "mail.list",
        "lookxy_read" => "mail.read",
        "lookxy_search" => "mail.search",
        "lookxy_mark" => "mail.mark",
        "lookxy_flag" => "mail.flag",
        "lookxy_move" => "mail.move",
        "lookxy_delete" => "mail.delete",
        "lookxy_attachments" => "mail.attachments",
        "lookxy_save_attachment" => "mail.save-attachment",
        "lookxy_select" => "mail.select",
        "lookxy_refresh" => "mail.refresh",
        other => return Err(format!("unknown tool: {other}")),
    };
    let client = client::resolve_target(&dir, "lookxy", args.get_str("target"))?;
    // Control verbs ignore unknown keys, so forwarding `arguments` verbatim
    // (including any `target`) is harmless.
    let result = client.call(verb, args.clone())?;
    Ok(result.to_string())
}

const TARGET_DESC: &str = "Optional: which lookxy to act on (a substring of its instance/pane id) \
     when several are open.";

fn tool_defs() -> Json {
    let target = || ("target", prop("string", TARGET_DESC));
    Json::Arr(vec![
        tool(
            "lookxy_list",
            "List the lookxy mail clients currently running on this machine (instance/pane id, port, pid).",
            vec![],
            &[],
        ),
        tool(
            "lookxy_status",
            "Report the account, sync state, folder/unread counts, pending outbox ops, and current selection.",
            vec![target()],
            &[],
        ),
        tool(
            "lookxy_folders",
            "List the mail folders (id, name, unread/total counts, well-known name).",
            vec![target()],
            &[],
        ),
        tool(
            "lookxy_messages",
            "List messages in a folder (defaults to the currently selected folder), newest first, paginated.",
            vec![
                (
                    "folder",
                    prop(
                        "string",
                        "Folder id to list (default: the currently selected folder).",
                    ),
                ),
                (
                    "limit",
                    prop("integer", "Max messages to return (default 50)."),
                ),
                (
                    "offset",
                    prop("integer", "Number of messages to skip (default 0)."),
                ),
                target(),
            ],
            &[],
        ),
        tool(
            "lookxy_read",
            "Read a message's full metadata and rendered plain-text body. If the body isn't cached \
             yet, requests a fetch and returns `body_pending:true`.",
            vec![("id", prop("string", "Message id.")), target()],
            &["id"],
        ),
        tool(
            "lookxy_search",
            "Full-text search the local mailbox (subject/sender/body) and return matching message summaries.",
            vec![
                ("query", prop("string", "Search text.")),
                (
                    "limit",
                    prop("integer", "Max messages to return (default 50)."),
                ),
                target(),
            ],
            &["query"],
        ),
        tool(
            "lookxy_mark",
            "Mark a message read or unread. Applies immediately in the local store and queues the \
             change for sync.",
            vec![
                ("id", prop("string", "Message id.")),
                (
                    "read",
                    prop("boolean", "true to mark read, false to mark unread."),
                ),
                target(),
            ],
            &["id", "read"],
        ),
        tool(
            "lookxy_flag",
            "Flag or unflag a message for follow-up. Applies immediately in the local store and \
             queues the change for sync.",
            vec![
                ("id", prop("string", "Message id.")),
                ("flagged", prop("boolean", "true to flag, false to unflag.")),
                target(),
            ],
            &["id", "flagged"],
        ),
        tool(
            "lookxy_move",
            "Move a message to another folder. Applies immediately in the local store and queues \
             the change for sync.",
            vec![
                ("id", prop("string", "Message id.")),
                ("dest", prop("string", "Destination folder id.")),
                target(),
            ],
            &["id", "dest"],
        ),
        tool(
            "lookxy_delete",
            "Delete a message. Applies immediately in the local store and queues the change for sync.",
            vec![("id", prop("string", "Message id.")), target()],
            &["id"],
        ),
        tool(
            "lookxy_attachments",
            "List a message's attachments (id, name, content type, size). Requests a metadata fetch \
             if none are cached yet but the message reports attachments.",
            vec![("id", prop("string", "Message id.")), target()],
            &["id"],
        ),
        tool(
            "lookxy_save_attachment",
            "Save an attachment to disk (queued; defaults to the Downloads folder).",
            vec![
                ("id", prop("string", "Message id.")),
                ("attachment", prop("string", "Attachment id.")),
                (
                    "dest",
                    prop(
                        "string",
                        "Destination file path (default: Downloads folder).",
                    ),
                ),
                target(),
            ],
            &["id", "attachment"],
        ),
        tool(
            "lookxy_select",
            "Move the TUI's selection so the open pane reflects a folder and/or message (either can \
             be omitted to leave it unchanged).",
            vec![
                ("folder", prop("string", "Folder id to select.")),
                ("id", prop("string", "Message id to open.")),
                target(),
            ],
            &[],
        ),
        tool(
            "lookxy_refresh",
            "Trigger a background sync refresh against Exchange.",
            vec![target()],
            &[],
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_list_includes_the_expected_tools() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t.get_str("name")).collect();
        for expected in [
            "lookxy_list",
            "lookxy_status",
            "lookxy_folders",
            "lookxy_messages",
            "lookxy_read",
            "lookxy_search",
            "lookxy_mark",
            "lookxy_flag",
            "lookxy_move",
            "lookxy_delete",
            "lookxy_attachments",
            "lookxy_save_attachment",
            "lookxy_select",
            "lookxy_refresh",
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
        let err = do_tool("lookxy_nonesuch", &Json::obj(vec![])).unwrap_err();
        assert!(err.contains("unknown tool"));
    }

    #[test]
    fn list_running_shape_is_stable() {
        // With no lookxy running (or no ctl dir), the list is present and empty-ish.
        let v = do_tool("lookxy_list", &Json::obj(vec![])).unwrap();
        assert!(v.contains("\"running\":["));
    }
}
