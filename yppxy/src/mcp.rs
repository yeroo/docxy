//! yppxy's [MCP](https://modelcontextprotocol.io) stdio server: exposes the
//! control verbs as native tools for an MCP client such as Claude Code
//! (`claude mcp add yppxy -- yppxy --mcp`).
//!
//! A thin adapter over a running yppxy's control surface (via
//! [`ctlcore::client`]); the protocol scaffolding lives in [`ctlcore::mcp`].
//! The MCP process opens no project of its own — it finds the yppxy the user
//! already has open and forwards tool calls to it, so edits land on that
//! editor's live schedule and undo stack.

use ctlcore::client;
use ctlcore::json::Json;
use ctlcore::mcp::{McpServer, prop, tool};

/// Serve MCP over stdio until stdin closes.
pub fn run() -> std::io::Result<()> {
    McpServer {
        name: "yppxy",
        version: env!("CARGO_PKG_VERSION"),
        tools: tool_defs(),
        handler: &do_tool,
    }
    .run()
}

/// Execute a tool by forwarding to the control surface.
fn do_tool(name: &str, args: &Json) -> Result<String, String> {
    let dir = ctlcore::config_ctl_dir("yppxy").ok_or("no control directory on this system")?;
    if name == "yppxy_list" {
        return Ok(client::list_running(&dir, "yppxy").to_string());
    }
    let verb = match name {
        "yppxy_status" => "proj.path",
        "yppxy_tasks" => "task.list",
        "yppxy_get" => "task.get",
        "yppxy_set" => "task.set",
        "yppxy_add" => "task.add",
        "yppxy_del" => "task.del",
        "yppxy_link" => "link.add",
        "yppxy_unlink" => "link.del",
        "yppxy_find" => "find",
        "yppxy_save" => "proj.save",
        other => return Err(format!("unknown tool: {other}")),
    };
    let client = client::resolve_target(&dir, "yppxy", args.get_str("target"))?;
    let result = client.call(verb, args.clone())?;
    Ok(result.to_string())
}

const TARGET_DESC: &str =
    "Optional: which yppxy to act on (a substring of its instance/pane id) when several are open.";

fn tool_defs() -> Json {
    let target = || ("target", prop("string", TARGET_DESC));
    let uid = || ("uid", prop("integer", "The task's UID (from yppxy_tasks)."));
    Json::Arr(vec![
        tool(
            "yppxy_list",
            "List the yppxy editors currently running on this machine (instance/pane id, port, pid).",
            vec![],
            &[],
        ),
        tool(
            "yppxy_status",
            "Report the open project's path, modified flag, task count, and scheduled start/finish.",
            vec![target()],
            &[],
        ),
        tool(
            "yppxy_tasks",
            "List every task of the live schedule (including unsaved edits): uid, name, outline \
             level, duration, scheduled start/finish, critical flag, slack, and predecessors.",
            vec![target()],
            &[],
        ),
        tool(
            "yppxy_get",
            "Read one task by UID.",
            vec![uid(), target()],
            &["uid"],
        ),
        tool(
            "yppxy_set",
            "Edit a task: rename, change duration (\"3d\", \"4h\", \"2w\"; \"0d\" = milestone), \
             or change outline level (1..20). Undoable; the plan reschedules.",
            vec![
                uid(),
                ("name", prop("string", "New task name.")),
                ("duration", prop("string", "New duration, e.g. \"3d\".")),
                ("level", prop("integer", "New outline level (1..20).")),
                target(),
            ],
            &["uid"],
        ),
        tool(
            "yppxy_add",
            "Insert a new task after the task with uid `after` (or append at the end). Returns \
             the new task with its uid. Undoable.",
            vec![
                (
                    "after",
                    prop(
                        "integer",
                        "UID of the task to insert after (default: append).",
                    ),
                ),
                ("name", prop("string", "Task name (default \"New task\").")),
                (
                    "duration",
                    prop("string", "Duration, e.g. \"3d\" (default 1 day)."),
                ),
                target(),
            ],
            &[],
        ),
        tool(
            "yppxy_del",
            "Delete a task by UID (links pointing at it are dropped). Undoable.",
            vec![uid(), target()],
            &["uid"],
        ),
        tool(
            "yppxy_link",
            "Make task `uid` depend on task `pred` (type FS/SS/FF/SF, default FS; optional lag \
             like \"1d\"). Undoable; the plan reschedules.",
            vec![
                uid(),
                ("pred", prop("integer", "UID of the predecessor task.")),
                (
                    "type",
                    prop("string", "Link type: FS, SS, FF, or SF (default FS)."),
                ),
                (
                    "lag",
                    prop("string", "Lag duration, e.g. \"1d\" (default none)."),
                ),
                target(),
            ],
            &["uid", "pred"],
        ),
        tool(
            "yppxy_unlink",
            "Remove the dependency of task `uid` on task `pred`. Undoable.",
            vec![
                uid(),
                ("pred", prop("integer", "UID of the predecessor to unlink.")),
                target(),
            ],
            &["uid", "pred"],
        ),
        tool(
            "yppxy_find",
            "Find tasks whose name contains the query (case-insensitive).",
            vec![("query", prop("string", "Text to search for.")), target()],
            &["query"],
        ),
        tool(
            "yppxy_save",
            "Save the open project to its file (or to `path` for save-as).",
            vec![
                ("path", prop("string", "Optional new file path (save-as).")),
                target(),
            ],
            &[],
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_list_includes_the_schedule_verbs() {
        let defs = tool_defs();
        let tools = defs.as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t.get_str("name")).collect();
        for expected in [
            "yppxy_list",
            "yppxy_tasks",
            "yppxy_set",
            "yppxy_add",
            "yppxy_link",
            "yppxy_save",
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
        let err = do_tool("yppxy_nonesuch", &Json::obj(vec![])).unwrap_err();
        assert!(err.contains("unknown tool"));
    }
}
