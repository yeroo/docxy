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
use ctlcore::mcp::{McpServer, item_ty, prop, prop_array, tool};
use std::path::PathBuf;

/// Serve MCP over stdio until stdin closes.
pub fn run() -> std::io::Result<()> {
    McpServer {
        name: "yppxy",
        version: buildinfo::long_version(env!("CARGO_PKG_VERSION")),
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
    let verb = tool_verb(name).ok_or_else(|| format!("unknown tool: {name}"))?;
    let (client, app) = client::resolve_target_in(sources, args.get_str("target"))?;
    if app == "yppxy" && args.get("tab").is_some() {
        return Err("'tab' is only supported by suite instances; yppxy has one project".into());
    }
    let result = client.call(verb, args.clone())?;
    Ok(result.to_string())
}

/// The control verb a tool forwards to.
fn tool_verb(name: &str) -> Option<&'static str> {
    Some(match name {
        "yppxy_status" => "proj.path",
        "yppxy_tasks" => "task.list",
        "yppxy_get" => "task.get",
        "yppxy_fields" => "task.fields",
        "yppxy_set" => "task.set",
        "yppxy_add" => "task.add",
        "yppxy_del" => "task.del",
        "yppxy_link" => "link.add",
        "yppxy_unlink" => "link.del",
        "yppxy_find" => "find",
        "yppxy_save" => "proj.save",
        "yppxy_assign_list" => "assign.list",
        "yppxy_assign_get" => "assign.get",
        "yppxy_assign_fields" => "assign.fields",
        "yppxy_assign_add" => "assign.add",
        "yppxy_assign_set" => "assign.set",
        "yppxy_assign_del" => "assign.del",
        _ => return None,
    })
}

const FIELDS_DESC: &str = "Optional: Project field names to read for each task returned, e.g. [\"% Complete\", \"Actual Start\", \"Baseline Finish\", \"Total Slack\"] (yppxy_fields lists them). Each comes back under `fields` as {text: as the sheet shows it, value: dates \"YYYY-MM-DD HH:MM\", durations, work and slack in minutes, money a number, null when unset}. An unknown name fails the call before any edit.";

const ASSIGN_FIELDS_DESC: &str = "Optional: Project assignment field names to read for each assignment returned, e.g. [\"Work\", \"Actual Work\", \"Baseline Cost\", \"Units\"] (yppxy_assign_fields lists them). Each comes back under `fields` as {text: as the sheet shows it, value: dates \"YYYY-MM-DD HH:MM\", work and delay in minutes (unlike work_hours/delay_hours and the work/delay arguments, which are hours), money a number, units a number, null when unset}. Custom fields (Text1, Number1, Cost1, Flag1, Date1, Duration1, ...) read the same way: Text a string, Number and Cost a number, Flag a bool, dates as above, Duration in minutes. An unknown name fails the call before any edit.";

const UNITS_DESC: &str = "Assignment units: a fraction (0.5) or a percent string (\"50%\"); for a material resource, its quantity.";

const TARGET_DESC: &str = "Optional: which yppxy or suite instance to act on (a substring of its instance/pane id) when several are open.";

fn tool_defs() -> Json {
    let target = || ("target", prop("string", TARGET_DESC));
    let tab = || {
        (
            "tab",
            prop_types(
                &["integer", "string"],
                "Suite only: absolute zero-based tab index or case-insensitive Project title/path substring. Omit for the active tab.",
            ),
        )
    };
    let uid = || ("uid", prop("integer", "The task's UID (from yppxy_tasks)."));
    let fields = || ("fields", prop_array(item_ty("string"), FIELDS_DESC));
    let assign_uid = || {
        (
            "uid",
            prop(
                "integer",
                "The assignment's own UID (from yppxy_assign_list).",
            ),
        )
    };
    let assign_fields = || ("fields", prop_array(item_ty("string"), ASSIGN_FIELDS_DESC));
    let units = || ("units", prop_types(&["number", "string"], UNITS_DESC));
    let work = || {
        (
            "work",
            prop_types(
                &["number", "string"],
                "Assignment work: a number of hours (a bare number is hours, even as a string such as \"8\"), or a duration such as \"40h\" or \"5d\".",
            ),
        )
    };
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
             warning (true when its subtasks finish after it, or it finishes after its parent \
             manual summary). Each task also has its row id and outline_number; pass `fields` \
             to read more.",
            vec![fields(), target(), tab()],
            &[],
        ),
        tool(
            "yppxy_get",
            "Read one task by UID; pass `fields` to read any Project field (progress, actuals, \
             work, cost, baselines, variances, slack, constraints, ...) as shown and as a value.",
            vec![uid(), fields(), target(), tab()],
            &["uid"],
        ),
        tool(
            "yppxy_fields",
            "List the Project field names `fields` can read, so a missing one is known to be \
             unsupported rather than empty.",
            vec![target(), tab()],
            &[],
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
                fields(),
                target(),
                tab(),
            ],
            &["uid"],
        ),
        tool(
            "yppxy_add",
            "Insert a new task after the task with uid `after` (or append at the end). When a \
             finish-to-start link joins `after` to the next task, the new task is linked into \
             that chain (A->B becomes A->N->B) while the plan's Autolink is on (the default). \
             Returns the new task with its uid. Undoable.",
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
                    prop(
                        "string",
                        "Duration, e.g. \"3d\" (default 1 day, estimated unless the plan's \
                         NewTasksEstimated is off).",
                    ),
                ),
                fields(),
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
                fields(),
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
                fields(),
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
                fields(),
                target(),
                tab(),
            ],
            &["query"],
        ),
        tool(
            "yppxy_save",
            "Save the open project to its file (or to `path` for save-as). Only .yppx/.xml are writable; extensionless paths gain .yppx. As in Project, a successful save clears undo and redo, so edits made before it can no longer be undone.",
            vec![
                ("path", prop("string", "Optional new file path (save-as).")),
                target(),
                tab(),
            ],
            &[],
        ),
        tool(
            "yppxy_assign_list",
            "List resource assignments of the live schedule: every one, or those of task `uid` \
             and/or of `resource` (a resource uid, or a name matched ignoring ASCII case). Each \
             assignment has its own uid (which the other yppxy_assign_* tools take), task, \
             resource, resource_name, units (a fraction, 1.0 = 100%; a material's quantity), \
             work_hours, regular/overtime work, cost, rate_table (a letter, A when unset), \
             baseline, actual and remaining work and cost, percent_work_complete, its own \
             start/finish, delay_hours and contour; pass `fields` to read more.",
            vec![
                (
                    "uid",
                    prop(
                        "integer",
                        "Optional: only this task's assignments (the task's UID, from yppxy_tasks).",
                    ),
                ),
                (
                    "resource",
                    prop_types(
                        &["integer", "string"],
                        "Optional: only this resource's assignments (a resource uid, or a name matched ignoring ASCII case).",
                    ),
                ),
                assign_fields(),
                target(),
                tab(),
            ],
            &[],
        ),
        tool(
            "yppxy_assign_get",
            "Read one assignment by its own UID; pass `fields` to read any Project assignment \
             field as shown and as a value.",
            vec![assign_uid(), assign_fields(), target(), tab()],
            &["uid"],
        ),
        tool(
            "yppxy_assign_fields",
            "List the Project assignment field names `fields` can read on the yppxy_assign_* \
             tools, so a missing one is known to be unsupported rather than empty.",
            vec![target(), tab()],
            &[],
        ),
        tool(
            "yppxy_assign_add",
            "Assign a resource to task `task`. `resource` is a resource uid, or a name: a new \
             name is created as a work resource, but a new name that is a number or contains a \
             comma is refused (pass a uid as a number). Returns the new assignment with its \
             uid. One undo step; the task reschedules.",
            vec![
                (
                    "task",
                    prop("integer", "The task's UID (from yppxy_tasks)."),
                ),
                (
                    "resource",
                    prop_types(
                        &["integer", "string"],
                        "The resource: its uid, or its name (a new name creates a work resource).",
                    ),
                ),
                units(),
                work(),
                assign_fields(),
                target(),
                tab(),
            ],
            &["task", "resource"],
        ),
        tool(
            "yppxy_assign_set",
            "Edit an assignment: units, work, overtime work, work contour, cost rate table, \
             delay, or actuals (actual work, or percent work complete). The task is rescheduled \
             by its type, as in Project: a units edit on a Fixed Units task keeps the work and \
             moves the duration; on Fixed Duration the work follows the units, and units given \
             together with work are recomputed from the work. Overtime (a work resource's, at \
             most the work; a work edit clears it, so given together it applies after the work) \
             is priced at the overtime rate but does not shorten the task. A contour keeps the \
             work, units and duration, and the assignment spans its task. Actual work (at most \
             the work) sets the actual start, at the whole work the actual finish, prices the \
             actual cost by the resource's accrual and derives remaining work, remaining cost \
             and percent work complete; it does not update the task's own progress. A contour \
             or an actual start ends a delayed assignment's extension of its task. Every \
             argument is checked before the edit, so a rejected call changes nothing. Returns \
             the updated assignment. One undo step.",
            vec![
                assign_uid(),
                units(),
                work(),
                (
                    "rate_table",
                    prop("string", "Cost rate table: a letter \"A\" to \"E\"."),
                ),
                (
                    "delay",
                    prop_types(
                        &["number", "string"],
                        "Assignment delay: a number of hours (a bare number is hours, even as a string), or a duration such as \"5d\".",
                    ),
                ),
                (
                    "overtime_work",
                    prop_types(
                        &["number", "string"],
                        "Overtime work, part of the work: a number of hours, or a duration such as \"1d\". A work resource's only.",
                    ),
                ),
                (
                    "contour",
                    prop_types(
                        &["string", "integer"],
                        "Work contour: Flat, Back Loaded, Front Loaded, Double Peak, Early Peak, Late Peak, Bell or Turtle (ASCII case ignored), or its code 0 to 7. Contoured is refused.",
                    ),
                ),
                (
                    "actual_work",
                    prop_types(
                        &["number", "string"],
                        "Actual work, at most the work: a number of hours, or a duration such as \"2d\". 0 clears the actual dates.",
                    ),
                ),
                (
                    "percent_work_complete",
                    prop_types(
                        &["integer", "string"],
                        "Instead of actual_work: the actual work as a whole percent of the work, 0 to 100, or \"50%\".",
                    ),
                ),
                assign_fields(),
                target(),
                tab(),
            ],
            &["uid"],
        ),
        tool(
            "yppxy_assign_del",
            "Remove an assignment by its own UID; the reply is {deleted, task}. One undo step; \
             the task reschedules.",
            vec![assign_uid(), target(), tab()],
            &["uid"],
        ),
    ])
}

/// A JSON-schema property that accepts any of `types`.
fn prop_types(types: &[&str], desc: &str) -> Json {
    Json::obj(vec![
        (
            "type",
            Json::Arr(types.iter().map(|t| Json::Str((*t).into())).collect()),
        ),
        ("description", Json::Str(desc.into())),
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
            "yppxy_assign_list",
            "yppxy_assign_get",
            "yppxy_assign_fields",
            "yppxy_assign_add",
            "yppxy_assign_set",
            "yppxy_assign_del",
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
    fn task_tools_take_fields_and_yppxy_fields_lists_them() {
        assert_eq!(tool_verb("yppxy_fields"), Some("task.fields"));
        assert_eq!(tool_verb("yppxy_nope"), None);
        for (name, verb) in [
            ("yppxy_assign_list", "assign.list"),
            ("yppxy_assign_get", "assign.get"),
            ("yppxy_assign_fields", "assign.fields"),
            ("yppxy_assign_add", "assign.add"),
            ("yppxy_assign_set", "assign.set"),
            ("yppxy_assign_del", "assign.del"),
        ] {
            assert_eq!(tool_verb(name), Some(verb));
        }
        let defs = tool_defs();
        for tool in defs.as_array().unwrap() {
            let name = tool.get_str("name").unwrap();
            if name != "yppxy_list" {
                assert!(tool_verb(name).is_some(), "{name} forwards nowhere");
            }
            let props = tool.get("inputSchema").unwrap().get("properties").unwrap();
            let verb = tool_verb(name);
            let assign = matches!(
                verb,
                Some("assign.list" | "assign.get" | "assign.add" | "assign.set")
            );
            let takes = assign
                || matches!(
                    verb,
                    Some(
                        "task.list"
                            | "task.get"
                            | "task.set"
                            | "task.add"
                            | "link.add"
                            | "link.del"
                            | "find"
                    )
                );
            match props.get("fields") {
                Some(f) if takes => {
                    assert_eq!(f.get_str("type"), Some("array"), "{name}");
                    assert_eq!(f.get("items").unwrap().get_str("type"), Some("string"));
                    // Each points to the tool that lists its own field names.
                    let desc = f.get_str("description").unwrap();
                    let lister = if assign {
                        "yppxy_assign_fields"
                    } else {
                        "yppxy_fields"
                    };
                    assert!(desc.contains(lister), "{name}: {desc}");
                    assert_eq!(desc.contains("yppxy_assign_fields"), assign, "{name}");
                    // A field's work is minutes; the assign arguments are hours.
                    assert!(
                        desc.contains("work and delay in minutes") || !assign,
                        "{name}"
                    );
                }
                None if !takes => {}
                other => panic!("{name}: fields {other:?}"),
            }
        }
    }

    /// The input schema of tool `name`.
    fn schema_of(name: &str) -> Json {
        tool_defs()
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.get_str("name") == Some(name))
            .unwrap_or_else(|| panic!("no tool {name}"))
            .get("inputSchema")
            .unwrap()
            .clone()
    }

    fn prop_of<'a>(schema: &'a Json, prop: &str) -> &'a Json {
        schema.get("properties").unwrap().get(prop).unwrap()
    }

    fn types_of(schema: &Json, prop: &str) -> Vec<String> {
        match prop_of(schema, prop).get("type").unwrap() {
            Json::Str(ty) => vec![ty.clone()],
            Json::Arr(tys) => tys.iter().map(|t| t.as_str().unwrap().into()).collect(),
            other => panic!("{prop}: type {other:?}"),
        }
    }

    fn required_of(schema: &Json) -> Vec<String> {
        let required = schema.get("required").unwrap().as_array().unwrap();
        required
            .iter()
            .map(|r| r.as_str().unwrap().into())
            .collect()
    }

    #[test]
    fn assign_tools_take_the_types_the_verbs_accept() {
        let list = schema_of("yppxy_assign_list");
        assert_eq!(types_of(&list, "uid"), ["integer"]);
        // `uid` here filters by task, unlike the other assign tools.
        let task_uid = prop_of(&list, "uid").get_str("description").unwrap();
        assert!(task_uid.contains("yppxy_tasks"), "{task_uid}");
        assert_eq!(types_of(&list, "resource"), ["integer", "string"]);
        assert!(required_of(&list).is_empty());
        assert!(required_of(&schema_of("yppxy_assign_fields")).is_empty());
        for name in ["yppxy_assign_get", "yppxy_assign_set", "yppxy_assign_del"] {
            let schema = schema_of(name);
            assert_eq!(types_of(&schema, "uid"), ["integer"], "{name}");
            let own = prop_of(&schema, "uid").get_str("description").unwrap();
            assert!(own.contains("yppxy_assign_list"), "{name}: {own}");
            assert_eq!(required_of(&schema), ["uid"], "{name}");
        }
        let add = schema_of("yppxy_assign_add");
        assert_eq!(types_of(&add, "task"), ["integer"]);
        assert_eq!(types_of(&add, "resource"), ["integer", "string"]);
        assert_eq!(required_of(&add), ["task", "resource"]);
        let set = schema_of("yppxy_assign_set");
        for schema in [&add, &set] {
            assert_eq!(types_of(schema, "units"), ["number", "string"]);
            assert_eq!(types_of(schema, "work"), ["number", "string"]);
        }
        assert_eq!(types_of(&set, "delay"), ["number", "string"]);
        for hours in ["overtime_work", "actual_work"] {
            assert_eq!(types_of(&set, hours), ["number", "string"], "{hours}");
        }
        assert_eq!(types_of(&set, "contour"), ["string", "integer"]);
        assert!(prop_of(&set, "contour").get("enum").is_none());
        assert_eq!(
            types_of(&set, "percent_work_complete"),
            ["integer", "string"]
        );
        // The verb reads the letter in either case, so no enum narrows it.
        assert_eq!(types_of(&set, "rate_table"), ["string"]);
        assert!(prop_of(&set, "rate_table").get("enum").is_none());
    }

    #[test]
    fn assign_tool_forwards_to_its_verb() {
        let root = std::env::temp_dir().join(format!("yppxy-mcp-assign-{}", std::process::id()));
        let suite_dir = root.join("suite");
        let sources = [Source {
            dir: &suite_dir,
            app: "suite",
        }];
        let (suite, requests) = ctlcore::serve(&suite_dir, "suite-assign").unwrap();
        let args = Json::obj(vec![
            ("uid", Json::Num(7.0)),
            ("units", Json::Str("50%".into())),
            ("fields", Json::Arr(vec![Json::Str("Actual Work".into())])),
            ("tab", Json::Num(1.0)),
        ]);
        let sent = args.clone();
        let reply = Json::obj(vec![("uid", Json::Num(7.0)), ("units", Json::Num(0.5))]);
        let answer = reply.clone();
        let worker = std::thread::spawn(move || {
            let req = requests
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert_eq!(req.verb, "assign.set");
            assert_eq!(req.args, sent);
            req.reply_ok(answer);
        });
        let result = do_tool_in(&sources, "yppxy_assign_set", &args).unwrap();
        worker.join().unwrap();
        assert_eq!(Json::parse(&result).unwrap(), reply);
        drop(suite);
        std::fs::remove_dir(suite_dir).unwrap();
        std::fs::remove_dir(root).unwrap();
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
