//! The fixture's version target and user interface (the optional symbols
//! `projcore::plugin_abi` documents): one of everything the hosts render, so
//! their plumbing is covered end to end without any real plugin.
//!
//! - Ribbon: on the Task tab a group "Fixture" with a large button
//!   `fixture.hello`, a toggle `fixture.flag`, a toggle `fixture.plain` with
//!   no icon (its label is drawn, #1608; `fixture.flat` on the Gantt Chart
//!   Format tab is the same) and a gallery `fixture.palette` of two swatches
//!   (`fixture.blue`, `fixture.red`).
//!   On the Gantt Chart Format tab a second group "Fixture Percent" after
//!   "Fixture Format" declares `priority` 120 and holds five small labelled
//!   buttons (`fixture.pct0` to `fixture.pct100`, #1685).
//! - Menus: a row-menu item `fixture.rename` after "Notes...".
//! - Dialogs: a Task Information page "Fixture" with a multiline and a grid
//!   (and a header label drawn above every page, #1609), and a dialog
//!   `fixture.options` (a number and a label the state fills from the number
//!   and the first selected task's name in a `Numbers` group box, a grid of
//!   the plan's task names whose selected row the state keeps on the
//!   selected task (#1610), a sample box the state styles, a filling memo; a
//!   fixed size, two columns, Help bottom left, #1609; a Bump button) that
//!   `fixture.hello` opens; OK sends `fixture.hello` again with the values,
//!   Bump sends it with `button: "bump"` (one `task.set` op naming the task
//!   "Bumped by fixture", the dialog kept open), Help is the host's (#1609: the
//!   dialog names no `help` URL, so the host says its documentation is
//!   coming). A header text `fixture.title` sits above the page and the
//!   echo label repeats its value, which the state sees at open; the header
//!   text spans two columns beside a tag field. The modeless `fixture.sheet`
//!   has its buttons down the right (Help last, opening its `help` page) and
//!   a grid of six rows.
//! - Commands: `fixture.hello` (the dialog, then a status with its number),
//!   `fixture.flag`, `fixture.plain` and `fixture.flat` (each flips its
//!   toggle: a view switch of its own name the host stores in the plan's
//!   formatting, #1553), the swatches (set the palette,
//!   returned as `view.palette`), `fixture.rename` (one `task.set` op
//!   renaming the first selected task to "Renamed by fixture"), and
//!   `task-information` (the page's OK: a status with the note's length),
//!   and `fixture.format` (#1575: sets the plan's gridlines through
//!   `view.gridlines`: dotted red Gantt rows with every second one solid
//!   blue, no sheet rows, solid green sheet columns and a solid red project
//!   start line; a second press, seeing the view's Gantt Rows dotted, clears
//!   them all with null).
//!   and `fixture.bars` (#1696: sets two bar style rows through `view.bars`,
//!   a normal-task row 1 with Resource Names to the right and a not-summary
//!   row 2 with other marks, one `view.task_bars` override of task 1 and
//!   the `fixture.names` switch; a second press, seeing rows in the view,
//!   clears both with null and leaves the switch on).
//! - Overlay: a polyline with an arrowhead from the first bar's end to the
//!   second bar's start, a text label at the first bar, and a `selected N`
//!   label at the selected task's bar when the request names one (#1565).
//!   With the `fixture.names` switch on it also writes each row's Resource
//!   Names right of its bar, unless a bar style row of the view already has
//!   a text field of that name (#1696).

use crate::json::{Json, quote};
use crate::{OK, UNSUPPORTED_EXTENSION, YppxBuf, bytes, fail, give};
use std::sync::Mutex;

/// The hosts this build of the fixture is for: the workspace's 0.1.x.
const TARGET: &str = r#"{"suite":"0.1.*","yppxy":"0.1.*","built_against":"fixture"}"#;

const MANIFEST: &str = r##"{
  "ribbon":[{"tab":"Task","groups":[{"title":"Fixture","controls":[
    {"id":"fixture.hello","kind":"button","label":"Hello","tip":"Say hello from the fixture","keytip":"XH","icon":"comment","large":true},
    {"id":"fixture.flag","kind":"toggle","label":"Flag","tip":"A toggle the fixture keeps","keytip":"XF","icon":"lock"},
    {"id":"fixture.plain","kind":"toggle","label":"Plain","tip":"A toggle with no icon: its label is drawn (#1608)","keytip":"XP","icon":""},
    {"id":"fixture.palette","kind":"gallery","label":"Palette","tip":"Bar palettes","swatches":[
      {"id":"fixture.blue","label":"Blue","color":"#3366CC","preview":"fixture.blue"},
      {"id":"fixture.red","label":"Red","color":"#CC3333","preview":"fixture.red"}]},
    {"id":"fixture.bold","kind":"toggle","label":"Bold","tip":"Bold on the selected cells (the view's cells)","keytip":"XB","icon":"bold"},
    {"id":"fixture.sheet","kind":"button","label":"Sheet (fixture)","tip":"A modeless dialog with a grid and buttons of its own","keytip":"XS","icon":"table"},
    {"id":"fixture.bars","kind":"button","label":"Bars (fixture)","tip":"Sets or resets the view's bar style rows and one task's bar override (#1696)","keytip":"XR","icon":"table"}]}]},
    {"tab":"Gantt Chart Format","groups":[{"title":"Fixture Format","before":"Bar Styles","controls":[
    {"id":"fixture.format","kind":"button","label":"Format (fixture)","tip":"A group placed before Bar Styles; sets or clears the plan's gridlines","icon":"font-name"},
    {"id":"fixture.flat","kind":"toggle","label":"Flat","tip":"A toggle with no icon on a tab that fits: its drawn label is measured (#1608)","icon":""}]},
    {"title":"Fixture Percent","after":"Fixture Format","priority":120,"controls":[
    {"id":"fixture.pct0","kind":"button","label":"0%","tip":"Five small labelled buttons stack three deep (#1685)"},
    {"id":"fixture.pct25","kind":"button","label":"25%"},
    {"id":"fixture.pct50","kind":"button","label":"50%"},
    {"id":"fixture.pct75","kind":"button","label":"75%"},
    {"id":"fixture.pct100","kind":"button","label":"100% Complete"}]}]}],
  "menus":[{"menu":"row","items":[{"id":"fixture.rename","label":"Rename (fixture)","icon":"comment","after":"Notes..."}]}],
  "dialogs":[
    {"id":"task-information","header":[{"name":"fixture.head","label":"Fixture head:","kind":"label","value":"on every page"}],
     "pages":[{"name":"Fixture","columns":2,"controls":[
      {"name":"fixture.note","label":"Fixture &note:","kind":"multiline","value":""},
      {"name":"fixture.grid","label":"Fixture items","kind":"grid","columns":[{"name":"Name","kind":"text"},{"name":"Done","kind":"checkbox"}],"value":[["a","false"],["b","true"]]},
      {"name":"fixture.pick","label":"Fixture p&ick:","kind":"dropdown","items":["none"],"value":"none"},
      {"name":"fixture.locked","label":"Fixture locked","kind":"checkbox","value":true}]}]},
    {"id":"fixture.options","title":"Fixture Options","size":[460,400],"header":[{"name":"fixture.title","label":"&Title:","kind":"text","value":"Fixture","span":2},{"name":"fixture.tag","label":"Ta&g:","kind":"text","value":""}],"pages":[{"name":"General","columns":2,"controls":[
      {"name":"n","label":"&Number:","kind":"number","value":"1","group":"Numbers"},
      {"name":"fixture.echo","label":"","kind":"label","value":"","group":"Numbers"},
      {"name":"fixture.tasks","label":"Tasks","kind":"grid","columns":[{"name":"Name","kind":"text"}],"value":[],"min_rows":3},
      {"name":"fixture.sample","label":"Sample","kind":"sample","value":{"text":"AaBbYyZz"}},
      {"name":"fixture.memo","label":"&Memo:","kind":"multiline","value":"","span":2,"fill":true}]},
      {"name":"Styles","columns":3,"controls":[
      {"name":"fixture.font","label":"&Font:","kind":"list","items":["Calibri","Segoe UI","Times New Roman"],"value":"Segoe UI"},
      {"name":"fixture.style","label":"Font st&yle:","kind":"list","items":["Regular","Italic","Bold","Bold Italic"],"value":"Regular"},
      {"name":"fixture.size","label":"&Size:","kind":"list","items":["8","9","10","11","12","14"],"value":"11"}]}],
     "buttons":["OK","Cancel",{"id":"help","label":"Help","cancel":true},{"id":"bump","label":"&Bump"}]},
    {"id":"fixture.sheet","title":"Fixture Sheet","modeless":true,"placement":"right","help":"https://example.test/fixture-help","pages":[{"name":"Sheet","controls":[
      {"name":"fixture.rows","label":"Tasks","kind":"grid","commit_command":true,"min_rows":6,"columns":[{"name":"Pick","kind":"check"},{"name":"Name","kind":"text"},{"name":"Units","kind":"number"}],"value":[]}]}],
     "buttons":[{"id":"apply","label":"&Apply","default":true},{"id":"clear","label":"Clear"},{"id":"close","label":"Close","cancel":true},"Help"]}],
  "palettes":[
    {"id":"fixture.blue","label":"Blue","bars":[{"kind":"on-track","color":"#3366CC","shape":"bar"},{"kind":"critical","color":"#1F3763","shape":"bar"}]},
    {"id":"fixture.red","label":"Red","bars":[{"kind":"on-track","color":"#CC3333","shape":"bar"},{"kind":"critical","color":"#7F0000","shape":"bar"}]}],
  "text_style_categories":["Fixture Tasks"]
}"##;

/// The chosen palette, kept across calls.
static PALETTE: Mutex<String> = Mutex::new(String::new());

/// The selection's first task and column, when the request has them.
fn cursor_cell(req: &Json) -> Option<(i64, String)> {
    let sel = req.get("selection")?;
    let uid = sel.get("uids")?.list().first()?.num()? as i64;
    let col = sel.get("cols")?.list().first()?.str()?.to_string();
    Some((uid, col))
}

/// Whether the cursor cell is bold in the view's `cells` (#1572).
fn bold(req: &Json) -> bool {
    let Some((uid, col)) = cursor_cell(req) else {
        return false;
    };
    req.get("view")
        .and_then(|v| v.get("cells"))
        .map(Json::list)
        .unwrap_or(&[])
        .iter()
        .find(|c| {
            c.get("uid").and_then(Json::num).map(|n| n as i64) == Some(uid)
                && c.get("column").and_then(Json::str) == Some(col.as_str())
        })
        .and_then(|c| c.get("bold"))
        .and_then(Json::bool)
        .unwrap_or(false)
}

/// The toggle's state: the `fixture.flag` switch of the request's view.
fn flag(req: &Json) -> bool {
    switch_on(req, "fixture.flag")
}

/// A named switch of the request's view (#1553); off when absent.
fn switch_on(req: &Json, name: &str) -> bool {
    req.get("view")
        .and_then(|v| v.get("switches"))
        .and_then(|s| s.get(name))
        .and_then(Json::bool)
        .unwrap_or(false)
}

fn palette() -> String {
    let p = PALETTE.lock().unwrap_or_else(|e| e.into_inner());
    if p.is_empty() {
        "fixture.blue".to_string()
    } else {
        p.clone()
    }
}

/// # Safety
/// `out` is a valid buffer pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_plugin_target(out: *mut YppxBuf) -> i32 {
    // SAFETY: the ABI's contract.
    unsafe { give(out, TARGET.as_bytes().to_vec()) };
    OK
}

#[unsafe(no_mangle)]
pub extern "C" fn yppx_ui_abi() -> u32 {
    1
}

/// # Safety
/// `out` is a valid buffer pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_ui_manifest(out: *mut YppxBuf) -> i32 {
    // SAFETY: the ABI's contract.
    unsafe { give(out, MANIFEST.as_bytes().to_vec()) };
    OK
}

/// The request's JSON, or the malformed-input code.
///
/// # Safety
/// `input` points at `len` bytes.
unsafe fn request(input: *const u8, len: usize) -> Result<Json, i32> {
    // SAFETY: the caller's contract.
    let text = std::str::from_utf8(unsafe { bytes(input, len) })
        .map_err(|_| fail(crate::MALFORMED_INPUT, "the request is not UTF-8"))?;
    Json::parse(text).map_err(|e| fail(crate::MALFORMED_INPUT, &format!("bad request JSON: {e}")))
}

/// Each `<Task>`'s first `<tag>` text, in order, read off the MSPDI text
/// (the fixture parses no plan).
fn task_field(req: &Json, tag: &str) -> Vec<String> {
    let plan = req.get("plan").and_then(Json::str).unwrap_or_default();
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = plan;
    while let Some(at) = rest.find("<Task>") {
        rest = &rest[at + 6..];
        let end = rest.find("</Task>").unwrap_or(rest.len());
        let task = &rest[..end];
        out.push(match task.find(&open) {
            Some(n) => {
                let after = &task[n + open.len()..];
                after[..after.find(&close).unwrap_or(after.len())].to_string()
            }
            None => String::new(),
        });
        rest = &rest[end..];
    }
    out
}

/// The plan's task names, in order.
fn task_names(req: &Json) -> Vec<String> {
    task_field(req, "Name")
}

/// The name of the plan's task `uid`.
fn task_name_of(req: &Json, uid: i64) -> Option<String> {
    let uids = task_field(req, "UID");
    let at = uids.iter().position(|u| u.trim() == uid.to_string())?;
    task_names(req).get(at).cloned()
}

/// A number control's value as text: the host sends what was typed, a
/// test may send a number.
fn number_text(n: &Json) -> String {
    match n {
        Json::Str(s) => s.clone(),
        Json::Num(x) => x.to_string(),
        _ => String::new(),
    }
}

/// The Fixture Sheet dialog's rows (#1602): one per task, `Pick` kept from
/// the open dialog's values by name, `Units` 100.
fn sheet_rows(req: &Json, values: Option<&Json>) -> String {
    let picked: Vec<String> = values
        .and_then(|v| v.get("fixture.rows"))
        .map(Json::list)
        .unwrap_or(&[])
        .iter()
        .filter(|row| row.list().first().and_then(Json::str) == Some("true"))
        .filter_map(|row| row.list().get(1).and_then(Json::str))
        .map(str::to_string)
        .collect();
    let rows: Vec<String> = task_names(req)
        .iter()
        .map(|name| format!(r#"["{}",{},"100"]"#, picked.contains(name), quote(name)))
        .collect();
    format!("[{}]", rows.join(","))
}

/// How many rows of the open Fixture Sheet are picked.
fn picked_count(values: Option<&Json>) -> usize {
    values
        .and_then(|v| v.get("fixture.rows"))
        .map(Json::list)
        .unwrap_or(&[])
        .iter()
        .filter(|row| row.list().first().and_then(Json::str) == Some("true"))
        .count()
}

/// The first selected uid of a request's `selection`.
fn first_uid(req: &Json) -> Option<i64> {
    req.get("selection")?
        .get("uids")?
        .list()
        .first()?
        .num()
        .map(|n| n as i64)
}

/// # Safety
/// The pointers follow the ABI: `input` `len` bytes, `out` a valid buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_ui_state(input: *const u8, len: usize, out: *mut YppxBuf) -> i32 {
    // SAFETY: the ABI's contract.
    let req = match unsafe { request(input, len) } {
        Ok(req) => req,
        Err(code) => return code,
    };
    let selected = first_uid(&req).is_some();
    let mut reply = format!(
        r#"{{"controls":{{"fixture.flag":{{"enabled":true,"checked":{}}},"fixture.plain":{{"enabled":true,"checked":{}}},"fixture.flat":{{"enabled":true,"checked":{}}},"fixture.bold":{{"enabled":{selected},"checked":{}}},"fixture.rename":{{"enabled":{selected}}}}},"gallery":{{"fixture.palette":{}}}"#,
        flag(&req),
        switch_on(&req, "fixture.plain"),
        switch_on(&req, "fixture.flat"),
        bold(&req),
        quote(&palette())
    );
    // A Task Information dialog about to open: the page's values.
    if let Some(dialog) = req.get("dialog") {
        // The modeless Fixture Sheet (#1602): its rows follow the plan (a
        // row per task, the picks kept by name) and Clear is enabled while
        // a row is picked, asked again on every change of its controls.
        if dialog.get("id").and_then(Json::str) == Some("fixture.sheet") {
            let values = dialog.get("values");
            reply.push_str(&format!(
                r#","values":{{"fixture.rows":{}}},"enabled":{{"clear":{}}}"#,
                sheet_rows(&req, values),
                picked_count(values) > 0
            ));
        }
        // Fixture Options (#1610): the label says the number typed and the
        // first selected task's name, so it follows the number (a control
        // change) and Bump's rename (the plan moved under a modal dialog).
        if dialog.get("id").and_then(Json::str) == Some("fixture.options") {
            let n = dialog
                .get("values")
                .and_then(|v| v.get("n"))
                .map_or_else(String::new, number_text);
            // The header's declared value reaches the state at open (#1609).
            let title = dialog
                .get("values")
                .and_then(|v| v.get("fixture.title"))
                .and_then(Json::str)
                .unwrap_or("?");
            let name = first_uid(&req)
                .and_then(|uid| task_name_of(&req, uid))
                .unwrap_or_default();
            // The grid: one row per task, the selected task's row selected
            // (a modal dialog's grid follows the plan, #1610); the sample
            // follows the number (odd: bold; size 12 + n, #1609).
            let names = task_names(&req);
            let rows: Vec<String> = names.iter().map(|n| format!("[{}]", quote(n))).collect();
            let selected = first_uid(&req)
                .and_then(|uid| {
                    task_field(&req, "UID")
                        .iter()
                        .position(|u| u.trim() == uid.to_string())
                })
                .map_or("null".to_string(), |i| i.to_string());
            let n_val: f64 = n.parse().unwrap_or(0.0);
            // Help is greyed while the number is 0 (#1609: the host refuses a
            // disabled Help like any button).
            reply.push_str(&format!(
                r#","values":{{"fixture.echo":{},"fixture.tasks":[{}],"fixture.tasks.selected_row":{selected},"fixture.sample":{{"text":"AaBbYyZz","bold":{},"size":{}}}}},"enabled":{{"help":{}}}"#,
                quote(&format!("Number {n} for {name} ({title})")),
                rows.join(","),
                n_val as i64 % 2 == 1,
                12 + (n_val as i64).clamp(0, 12),
                n_val != 0.0
            ));
        }
        if dialog.get("id").and_then(Json::str) == Some("task-information") {
            let uid = dialog.get("uid").and_then(Json::num).unwrap_or(0.0) as i64;
            // The values, the dropdown's items for this plan (#1556) and a
            // control greyed by the state.
            reply.push_str(&format!(
                r#","values":{{"fixture.note":{},"fixture.grid":[["task","false"],["{uid}","true"]],"fixture.pick":"{uid}"}},"items":{{"fixture.pick":["task","{uid}"]}},"enabled":{{"fixture.locked":false}}"#,
                quote(&format!("note for task {uid}"))
            ));
        }
    }
    reply.push('}');
    // SAFETY: the ABI's contract.
    unsafe { give(out, reply.into_bytes()) };
    OK
}

/// # Safety
/// The pointers follow the ABI: `input` `len` bytes, `out` a valid buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_ui_command(input: *const u8, len: usize, out: *mut YppxBuf) -> i32 {
    // SAFETY: the ABI's contract.
    let req = match unsafe { request(input, len) } {
        Ok(req) => req,
        Err(code) => return code,
    };
    let id = req.get("id").and_then(Json::str).unwrap_or_default();
    let reply = match id {
        // The gridlines (#1575): set them, or, seeing them set in the
        // view, put every line back to its default.
        "fixture.format" => {
            let set = req
                .get("view")
                .and_then(|v| v.get("gridlines"))
                .and_then(|g| g.get("Gantt Rows"))
                .and_then(|g| g.get("type"))
                .and_then(Json::str)
                == Some("dotted");
            if set {
                r#"{"ops":[],"status":"Fixture: format","view":{"gridlines":null}}"#.to_string()
            } else {
                r##"{"ops":[],"status":"Fixture: format","view":{"gridlines":{
                    "Gantt Rows":{"type":"dotted","color":"#CC3333","interval":2,"interval_type":"solid","interval_color":"#3366CC"},
                    "Sheet Rows":{"type":"none"},
                    "Sheet Columns":{"type":"solid","color":"#33AA33"},
                    "Project Start":{"type":"solid","color":"#CC3333"}}}}"##
                    .to_string()
            }
        }
        // The bar style rows (#1696): set them, or, seeing rows in the view,
        // clear the table and the overrides.
        "fixture.bars" => {
            let set = req
                .get("view")
                .and_then(|v| v.get("bars"))
                .map(Json::list)
                .is_some_and(|rows| !rows.is_empty());
            if set {
                r#"{"ops":[],"status":"Fixture: bars reset","view":{"bars":null,"task_bars":null}}"#
                    .to_string()
            } else {
                r##"{"ops":[],"status":"Fixture: bars","view":{"switches":{"fixture.names":true},
                    "bars":[
                      {"name":"Main","show_for":["Normal"],"row":1,
                       "start":{"shape":"diamond","type":"framed","color":"#CC3333"},
                       "middle":{"shape":"full","color":"#CC3333"},
                       "end":{"shape":"arrow-right","type":"solid"},
                       "text":{"right":"Resource Names"}},
                      {"name":"Detail","show_for":["Not Summary"],"row":2,
                       "start":{"shape":"square"},
                       "middle":{"shape":"narrow-middle","pattern":"light-fill","color":"#3366CC"},
                       "end":{"shape":"circle","type":"dashed"},
                       "text":{"inside":"Name"}}],
                    "task_bars":[{"uid":1,"middle":{"color":"#33AA33"},"text":{"top":"Overridden"}}]}}"##
                    .to_string()
            }
        }
        "fixture.hello" => match (req.get("button").and_then(Json::str), req.get("values")) {
            // Bump (#1610): one op on the first selected task, the dialog
            // kept open.
            (Some("bump"), _) => match first_uid(&req) {
                Some(uid) => format!(
                    r#"{{"ops":[{{"verb":"task.set","args":{{"uid":{uid},"name":"Bumped by fixture"}}}}],"status":{}}}"#,
                    quote(&format!("Fixture: bumped task {uid}"))
                ),
                None => r#"{"ops":[],"status":"Fixture: select a task first"}"#.to_string(),
            },
            (_, None) => {
                r#"{"ops":[],"dialog":"fixture.options","status":"Fixture: asking"}"#.to_string()
            }
            (_, Some(values)) => {
                let n = values.get("n").map_or_else(String::new, number_text);
                format!(
                    r#"{{"ops":[],"status":{}}}"#,
                    quote(&format!("Fixture: hello {n}"))
                )
            }
        },
        "fixture.bold" => {
            // Bold on every selected cell, off when the cursor cell is bold.
            // It also strikes the cell through and gives it a dark
            // diagonal pattern, `solid` when off (#1664).
            let on = !bold(&req);
            let uids: Vec<i64> = req
                .get("selection")
                .and_then(|s| s.get("uids"))
                .map(Json::list)
                .unwrap_or(&[])
                .iter()
                .filter_map(Json::num)
                .map(|n| n as i64)
                .collect();
            let cols: Vec<String> = req
                .get("selection")
                .and_then(|s| s.get("cols"))
                .map(Json::list)
                .unwrap_or(&[])
                .iter()
                .filter_map(Json::str)
                .map(str::to_string)
                .collect();
            if uids.is_empty() || cols.is_empty() {
                r#"{"ops":[],"status":"Fixture: select a cell first"}"#.to_string()
            } else {
                let cells: Vec<String> = uids
                    .iter()
                    .flat_map(|uid| {
                        cols.iter().map(move |col| {
                            format!(
                                r#"{{"uid":{uid},"column":{},"bold":{on},"strikethrough":{on},"pattern":"{}"}}"#,
                                quote(col),
                                if on { "dark-diagonal-left" } else { "solid" }
                            )
                        })
                    })
                    .collect();
                format!(
                    r#"{{"ops":[],"status":"Fixture: bold {}","view":{{"cells":[{}]}}}}"#,
                    if on { "on" } else { "off" },
                    cells.join(",")
                )
            }
        }
        // The Fixture Sheet (#1602): the button pressed sends the command
        // with its id; Apply reports and stays open, Clear closes.
        "fixture.sheet" => match req.get("button").and_then(Json::str) {
            // A committed cell (#1602): a Name cell renames its task at once.
            None if req.get("edit").is_some() => {
                let edit = req.get("edit").unwrap();
                let row = edit.get("row").and_then(Json::num).unwrap_or(0.0) as usize;
                let col = edit.get("col").and_then(Json::num).unwrap_or(0.0) as usize;
                let value = edit.get("value").and_then(Json::str).unwrap_or_default();
                let uid = task_field(&req, "UID")
                    .get(row)
                    .cloned()
                    .unwrap_or_default();
                if col == 1 && !uid.is_empty() {
                    format!(
                        r#"{{"ops":[{{"verb":"task.set","args":{{"uid":{uid},"name":{}}}}}],"status":{}}}"#,
                        quote(value),
                        quote(&format!("Fixture: renamed row {row}"))
                    )
                } else {
                    format!(
                        r#"{{"ops":[],"status":{}}}"#,
                        quote(&format!("Fixture: edited row {row} col {col} to {value}"))
                    )
                }
            }
            None => r#"{"ops":[],"dialog":"fixture.sheet","status":"Fixture: sheet"}"#.to_string(),
            Some("apply") => {
                let values = req.get("values");
                let row = values
                    .and_then(|v| v.get("fixture.rows.selected_row"))
                    .and_then(Json::num)
                    .map_or_else(|| "none".to_string(), |r| (r as i64).to_string());
                format!(
                    r#"{{"ops":[],"status":{}}}"#,
                    quote(&format!(
                        "Fixture: applied {} picked, row {row}",
                        picked_count(values)
                    ))
                )
            }
            Some(_) => r#"{"ops":[],"status":"Fixture: cleared","close":true}"#.to_string(),
        },
        // The percent buttons (#1685) only say which was pressed.
        "fixture.pct0" | "fixture.pct25" | "fixture.pct50" | "fixture.pct75" | "fixture.pct100" => {
            format!(
                r#"{{"ops":[],"status":"Fixture: {}"}}"#,
                id.trim_start_matches("fixture.")
            )
        }
        "fixture.flag" | "fixture.plain" | "fixture.flat" => {
            let on = !switch_on(&req, id);
            let word = match id {
                "fixture.flag" => "flag",
                "fixture.plain" => "plain",
                _ => "flat",
            };
            format!(
                r#"{{"ops":[],"status":"Fixture: {word} {}","view":{{"switches":{{"{id}":{on}}}}}}}"#,
                if on { "on" } else { "off" }
            )
        }
        "fixture.blue" | "fixture.red" => {
            *PALETTE.lock().unwrap_or_else(|e| e.into_inner()) = id.to_string();
            format!(
                r#"{{"ops":[],"status":{},"view":{{"palette":{}}}}}"#,
                quote(&format!("Fixture: palette {id}")),
                quote(id)
            )
        }
        // Task Information's OK with the fixture page's values: the note's
        // length, so a test can see the values arrive.
        "task-information" => {
            let note = req
                .get("values")
                .and_then(|v| v.get("fixture.note"))
                .and_then(Json::str)
                .unwrap_or_default();
            format!(
                r#"{{"ops":[],"status":{}}}"#,
                quote(&format!(
                    "Fixture: note saved ({} chars)",
                    note.chars().count()
                ))
            )
        }
        "fixture.rename" => match first_uid(&req) {
            Some(uid) => {
                // The selection's size shows the host kept the range (#1571).
                let selected = req
                    .get("selection")
                    .and_then(|s| s.get("uids"))
                    .map(|u| u.list().len())
                    .unwrap_or(1);
                let status = if selected > 1 {
                    format!("Fixture: renamed task {uid} of {selected} selected")
                } else {
                    format!("Fixture: renamed task {uid}")
                };
                format!(
                    r#"{{"ops":[{{"verb":"task.set","args":{{"uid":{uid},"name":"Renamed by fixture"}}}}],"status":{}}}"#,
                    quote(&status)
                )
            }
            None => r#"{"ops":[],"status":"Fixture: select a task first"}"#.to_string(),
        },
        _ => {
            return fail(
                UNSUPPORTED_EXTENSION,
                &format!("not a fixture command: {id}"),
            );
        }
    };
    // SAFETY: the ABI's contract.
    unsafe { give(out, reply.into_bytes()) };
    OK
}

/// # Safety
/// The pointers follow the ABI: `input` `len` bytes, `out` a valid buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_ui_overlay(input: *const u8, len: usize, out: *mut YppxBuf) -> i32 {
    // SAFETY: the ABI's contract.
    let req = match unsafe { request(input, len) } {
        Ok(req) => req,
        Err(code) => return code,
    };
    // The bars in view: (x, y, w, h) of the rows that have one.
    let bars: Vec<(f64, f64, f64, f64)> = req
        .get("rows")
        .map(Json::list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|row| {
            let bar = row.get("bar")?;
            if bar.is_null() {
                return None;
            }
            let f = |k: &str| bar.get(k).and_then(Json::num);
            Some((f("x")?, f("y")?, f("w")?, f("h")?))
        })
        .collect();
    let mut shapes = Vec::new();
    if let Some((x, y, _, _)) = bars.first() {
        shapes.push(format!(
            r##"{{"kind":"text","x":{x},"y":{},"text":"fixture","color":"#333333","size":10}}"##,
            y - 4.0
        ));
    }
    if let [(x0, y0, w0, h0), (x1, y1, _, h1), ..] = bars.as_slice() {
        let (xe, yc0) = (x0 + w0, y0 + h0 / 2.0);
        let yc1 = y1 + h1 / 2.0;
        shapes.push(format!(
            r##"{{"kind":"polyline","points":[[{xe},{yc0}],[{},{yc0}],[{},{yc1}]],"color":"#000000","width":1}}"##,
            xe + 6.0,
            xe + 6.0
        ));
        shapes.push(format!(
            r##"{{"kind":"arrow","from":[{},{yc1}],"to":[{x1},{yc1}],"color":"#000000","width":1}}"##,
            xe + 6.0
        ));
    }
    // The selected task's bar, when the request names one and it is shown.
    let selected = req
        .get("selected")
        .map(Json::list)
        .and_then(|l| l.first())
        .and_then(Json::num);
    if let Some(uid) = selected {
        let row = req
            .get("rows")
            .map(Json::list)
            .unwrap_or(&[])
            .iter()
            .find(|row| row.get("uid").and_then(Json::num) == Some(uid));
        if let Some(bar) = row.and_then(|r| r.get("bar")).filter(|b| !b.is_null()) {
            let f = |k: &str| bar.get(k).and_then(Json::num).unwrap_or(0.0);
            shapes.push(format!(
                r##"{{"kind":"text","x":{},"y":{},"text":"selected {}","color":"#333333","size":10}}"##,
                f("x") + f("w") + 6.0,
                f("y"),
                uid as i64
            ));
        }
    }
    // Resource Names right of each bar, as the fixture's own bar text,
    // unless a bar style row of the view writes them itself (#1696).
    let flags = req.get("flags");
    let names_on = flags
        .and_then(|f| f.get("switches"))
        .and_then(|s| s.get("fixture.names"))
        .and_then(Json::bool)
        .unwrap_or(false);
    let row_writes_names = flags
        .and_then(|f| f.get("bars"))
        .map(Json::list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|row| row.get("text"))
        .any(|text| {
            ["left", "right", "top", "bottom", "inside"]
                .iter()
                .any(|pos| text.get(pos).and_then(Json::str) == Some("Resource Names"))
        });
    if names_on && !row_writes_names {
        for row in req.get("rows").map(Json::list).unwrap_or(&[]) {
            let names = row
                .get("resource_names")
                .and_then(Json::str)
                .unwrap_or_default();
            let Some(bar) = row.get("bar").filter(|b| !b.is_null()) else {
                continue;
            };
            if names.is_empty() {
                continue;
            }
            let f = |k: &str| bar.get(k).and_then(Json::num).unwrap_or(0.0);
            shapes.push(format!(
                r##"{{"kind":"text","x":{},"y":{},"text":{},"color":"#333333","size":10}}"##,
                f("x") + f("w") + 6.0,
                f("y"),
                quote(&format!("names {names}"))
            ));
        }
    }
    let reply = format!(r#"{{"shapes":[{}]}}"#, shapes.join(","));
    // SAFETY: the ABI's contract.
    unsafe { give(out, reply.into_bytes()) };
    OK
}
