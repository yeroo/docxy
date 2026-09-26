//! yppxy's [MCP](https://modelcontextprotocol.io) stdio server: exposes the
//! control verbs as native tools for an MCP client such as Claude Code
//! (`claude mcp add yppxy -- yppxy --mcp`).
//!
//! A thin adapter over running yppxy and suite Project control surfaces (via
//! [`ctlcore::client`]); the protocol scaffolding lives in [`ctlcore::mcp`].
//! The MCP process opens no project of its own — it finds the editor the user
//! already has open and forwards tool calls to it, so edits land on that
//! editor's live schedule and undo stack.

use ctlcore::client::{self, Source};
use ctlcore::json::Json;
use ctlcore::mcp::{McpServer, prop, tool};
use std::path::PathBuf;

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
    let yppxy = ctlcore::config_ctl_dir("yppxy");
    let suite = suite_ctl_dir_from(std::env::var_os("DOCXY_CONFIG_DIR"), dirs::config_dir());
    let mut sources = vec![Source {
        dir: &suite,
        app: "suite",
    }];
    if let Some(dir) = &yppxy {
        sources.push(Source { dir, app: "yppxy" });
    }
    do_tool_in(&sources, name, args)
}

fn suite_ctl_dir_from(over: Option<std::ffi::OsString>, os_config: Option<PathBuf>) -> PathBuf {
    let root = match over {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => os_config.unwrap_or_else(|| PathBuf::from(".")),
    };
    root.join("suite").join("ctl")
}

fn do_tool_in(sources: &[Source<'_>], name: &str, args: &Json) -> Result<String, String> {
    if name == "yppxy_list" {
        return Ok(client::list_running_in(sources).to_string());
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
    let (client, app) = client::resolve_target_in(sources, args.get_str("target"))?;
    if app == "yppxy" && args.get("tab").is_some() {
        return Err("'tab' is only supported by suite instances; yppxy has one project".into());
    }
    let result = client.call(verb, args.clone())?;
    Ok(result.to_string())
}

const TARGET_DESC: &str = "Optional: which yppxy or suite instance to act on (a substring of its instance/pane id) when several are open.";

fn tool_defs() -> Json {
    let target = || ("target", prop("string", TARGET_DESC));
    let tab = || {
        let description = "Suite only: absolute zero-based tab index or case-insensitive Project title/path substring. Omit for the active tab.";
        (
            "tab",
            Json::obj(vec![
                (
                    "type",
                    Json::Arr(vec![
                        Json::Str("integer".into()),
                        Json::Str("string".into()),
                    ]),
                ),
                ("description", Json::Str(description.into())),
            ]),
        )
    };
    let uid = || ("uid", prop("integer", "The task's UID (from yppxy_tasks)."));
    Json::Arr(vec![
        tool(
            "yppxy_list",
            "List running yppxy and suite editors (instance/pane id, app, port, pid).",
            vec![],
            &[],
        ),
        tool(
            "yppxy_status",
            "Report the open project's path, modified flag, task count, and scheduled start/finish.",
            vec![target(), tab()],
            &[],
        ),
        tool(
            "yppxy_tasks",
            "List every task of the live schedule (including unsaved edits): uid, name, outline \
             level, manual (true = Manually Scheduled), duration, scheduled start/finish, critical \
             flag, slack, and predecessors. Summaries add rollup_start/rollup_finish, the span of \
             their subtasks; a manually scheduled summary keeps its own start/finish and adds \
             warning (true when its subtasks finish after it).",
            vec![target(), tab()],
            &[],
        ),
        tool(
            "yppxy_get",
            "Read one task by UID.",
            vec![uid(), target(), tab()],
            &["uid"],
        ),
        tool(
            "yppxy_set",
            "Edit a task: rename, change duration (\"3d\", \"4h\", \"2w\"; \"0d\" = milestone), \
             change outline level (1..20), or switch between Manually and Auto Scheduled \
             (\"manual\": true pins the task at its current dates; false lets the scheduler \
             place it). One undo step; the plan reschedules.",
            vec![
                uid(),
                ("name", prop("string", "New task name.")),
                ("duration", prop("string", "New duration, e.g. \"3d\".")),
                ("level", prop("integer", "New outline level (1..20).")),
                (
                    "manual",
                    prop(
                        "boolean",
                        "true = Manually Scheduled (pinned at its current dates), false = Auto Scheduled.",
                    ),
                ),
                target(),
                tab(),
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
                tab(),
            ],
            &[],
        ),
        tool(
            "yppxy_del",
            "Delete a task by UID; a summary takes its subtasks with it (reply `removed` lists \
             every UID). Links pointing at removed tasks are dropped. One undo step.",
            vec![uid(), target(), tab()],
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
                tab(),
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
                tab(),
            ],
            &["uid", "pred"],
        ),
        tool(
            "yppxy_find",
            "Find tasks whose name contains the query (case-insensitive).",
            vec![
                ("query", prop("string", "Text to search for.")),
                target(),
                tab(),
            ],
            &["query"],
        ),
        tool(
            "yppxy_save",
            "Save the open project to its file (or to `path` for save-as). Only .yppx/.xml are writable; extensionless paths gain .yppx.",
            vec![
                ("path", prop("string", "Optional new file path (save-as).")),
                target(),
                tab(),
            ],
            &[],
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_control_directory_matches_suite_config_policy() {
        let os = PathBuf::from("os-config");
        let suffix = PathBuf::from("suite").join("ctl");
        assert_eq!(suite_ctl_dir_from(None, Some(os.clone())), os.join(&suffix));
        assert_eq!(
            suite_ctl_dir_from(Some("".into()), Some(os.clone())),
            os.join(&suffix)
        );
        assert_eq!(
            suite_ctl_dir_from(Some("override".into()), Some(os)),
            PathBuf::from("override").join(&suffix)
        );
        assert_eq!(
            suite_ctl_dir_from(None, None),
            PathBuf::from(".").join(suffix)
        );
    }

    #[test]
    fn tab_is_optional_on_every_instance_tool_and_has_both_types() {
        for tool in tool_defs().as_array().unwrap() {
            let schema = tool.get("inputSchema").unwrap();
            let tab = schema.get("properties").unwrap().get("tab");
            if tool.get_str("name") == Some("yppxy_list") {
                assert!(tab.is_none());
            } else {
                assert_eq!(
                    tab.unwrap().get("type").unwrap().as_array().unwrap(),
                    &[Json::Str("integer".into()), Json::Str("string".into())]
                );
                assert!(
                    !schema
                        .get("required")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .contains(&Json::Str("tab".into()))
                );
            }
        }
    }

    #[test]
    fn bridge_forwards_tab_to_suite_and_rejects_it_for_yppxy() {
        let root = std::env::temp_dir().join(format!("yppxy-mcp-suite-{}", std::process::id()));
        let yppxy_dir = root.join("yppxy");
        let suite_dir = root.join("suite");
        let sources = [
            Source {
                dir: &yppxy_dir,
                app: "yppxy",
            },
            Source {
                dir: &suite_dir,
                app: "suite",
            },
        ];
        let (suite, requests) = ctlcore::serve(&suite_dir, "suite-left").unwrap();
        let worker = std::thread::spawn(move || {
            for _ in 0..2 {
                let req = requests
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                assert_eq!(req.verb, "task.list");
                let args = req.args.clone();
                req.reply_ok(args);
            }
        });
        for tab in [Json::Num(2.0), Json::Str("plan.xml".into())] {
            let args = Json::obj(vec![("tab", tab.clone())]);
            let result = do_tool_in(&sources, "yppxy_tasks", &args).unwrap();
            assert_eq!(Json::parse(&result).unwrap().get("tab"), Some(&tab));
        }
        worker.join().unwrap();
        let (yppxy, requests) = ctlcore::serve(&yppxy_dir, "yppxy-right").unwrap();
        let listing =
            Json::parse(&do_tool_in(&sources, "yppxy_list", &Json::Null).unwrap()).unwrap();
        assert_eq!(listing.get("running").unwrap().as_array().unwrap().len(), 2);
        let ambiguous = do_tool_in(&sources, "yppxy_tasks", &Json::Null).unwrap_err();
        assert!(ambiguous.contains("suite-left") && ambiguous.contains("yppxy-right"));
        let args = Json::obj(vec![
            ("target", Json::Str("right".into())),
            ("tab", Json::Num(0.0)),
        ]);
        assert!(
            do_tool_in(&sources, "yppxy_tasks", &args)
                .unwrap_err()
                .contains("only supported by suite")
        );
        assert!(matches!(
            requests.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        // Omitting tab preserves the standalone tool protocol.
        let worker = std::thread::spawn(move || {
            let req = requests
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert!(req.args.get("tab").is_none());
            req.reply_ok(Json::Bool(true));
        });
        assert_eq!(
            do_tool_in(
                &sources,
                "yppxy_tasks",
                &Json::obj(vec![("target", Json::Str("right".into()))])
            )
            .unwrap(),
            "true"
        );
        worker.join().unwrap();
        drop((suite, yppxy));
        std::fs::remove_dir(yppxy_dir).unwrap();
        std::fs::remove_dir(suite_dir).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

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
    fn descriptions_carry_no_source_indentation() {
        // A string broken across lines without a trailing `\` keeps the next
        // line's indentation, which tools/list would send as it is.
        let defs = tool_defs().to_string();
        assert!(!defs.contains("  "), "{defs}");
    }

    #[test]
    fn set_takes_the_task_mode_as_a_boolean() {
        let defs = tool_defs();
        let set = defs
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.get_str("name") == Some("yppxy_set"))
            .unwrap();
        let props = set.get("inputSchema").unwrap().get("properties").unwrap();
        assert_eq!(
            props.get("manual").unwrap().get_str("type"),
            Some("boolean")
        );
    }

    #[test]
    fn unknown_tool_is_reported() {
        let err = do_tool("yppxy_nonesuch", &Json::obj(vec![])).unwrap_err();
        assert!(err.contains("unknown tool"));
    }
}
