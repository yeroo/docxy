//! The fixture's version target and user interface (the optional symbols
//! `projcore::plugin_abi` documents): one of everything the hosts render, so
//! their plumbing is covered end to end without any real plugin.
//!
//! - Ribbon: on the Task tab a group "Fixture" with a large button
//!   `fixture.hello`, a toggle `fixture.flag` and a gallery `fixture.palette`
//!   of two swatches (`fixture.blue`, `fixture.red`).
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
//!   "Bumped by fixture", the dialog kept open), Help with `button: "help"`
//!   (a status). The modeless `fixture.sheet` has its buttons down the right
//!   and a grid of six rows.
//! - Commands: `fixture.hello` (the dialog, then a status with its number),
//!   `fixture.flag` (flips the toggle: a view switch of the same name the
//!   host stores in the plan's formatting, #1553), the swatches (set the palette,
//!   returned as `view.palette`), `fixture.rename` (one `task.set` op
//!   renaming the first selected task to "Renamed by fixture"), and
//!   `task-information` (the page's OK: a status with the note's length).
//! - Overlay: a polyline with an arrowhead from the first bar's end to the
//!   second bar's start, a text label at the first bar, and a `selected N`
//!   label at the selected task's bar when the request names one (#1565).

use crate::json::{Json, quote};
use crate::{OK, UNSUPPORTED_EXTENSION, YppxBuf, bytes, fail, give};
use std::sync::Mutex;

/// The hosts this build of the fixture is for: the workspace's 0.1.x.
const TARGET: &str = r#"{"suite":"0.1.*","yppxy":"0.1.*","built_against":"fixture"}"#;

const MANIFEST: &str = r##"{
  "ribbon":[{"tab":"Task","groups":[{"title":"Fixture","controls":[
    {"id":"fixture.hello","kind":"button","label":"Hello","tip":"Say hello from the fixture","keytip":"XH","icon":"comment","large":true},
    {"id":"fixture.flag","kind":"toggle","label":"Flag","tip":"A toggle the fixture keeps","keytip":"XF","icon":"lock"},
    {"id":"fixture.palette","kind":"gallery","label":"Palette","tip":"Bar palettes","swatches":[
      {"id":"fixture.blue","label":"Blue","color":"#3366CC","preview":"fixture.blue"},
      {"id":"fixture.red","label":"Red","color":"#CC3333","preview":"fixture.red"}]},
    {"id":"fixture.bold","kind":"toggle","label":"Bold","tip":"Bold on the selected cells (the view's cells)","keytip":"XB","icon":"bold"},
    {"id":"fixture.sheet","kind":"button","label":"Sheet (fixture)","tip":"A modeless dialog with a grid and buttons of its own","keytip":"XS","icon":"numbers"}]}]},
    {"tab":"Gantt Chart Format","groups":[{"title":"Fixture Format","before":"Bar Styles","controls":[
    {"id":"fixture.format","kind":"button","label":"Format (fixture)","tip":"A group placed before Bar Styles","icon":"font"}]}]}],
  "menus":[{"menu":"row","items":[{"id":"fixture.rename","label":"Rename (fixture)","icon":"comment","after":"Notes..."}]}],
  "dialogs":[
    {"id":"task-information","header":[{"name":"fixture.head","label":"Fixture head:","kind":"label","value":"on every page"}],
     "pages":[{"name":"Fixture","controls":[
      {"name":"fixture.note","label":"Fixture &note:","kind":"multiline","value":""},
      {"name":"fixture.grid","label":"Fixture items","kind":"grid","columns":[{"name":"Name","kind":"text"},{"name":"Done","kind":"checkbox"}],"value":[["a","false"],["b","true"]]},
      {"name":"fixture.pick","label":"Fixture p&ick:","kind":"dropdown","items":["none"],"value":"none"},
      {"name":"fixture.locked","label":"Fixture locked","kind":"checkbox","value":true}]}]},
    {"id":"fixture.options","title":"Fixture Options","size":[460,400],"pages":[{"name":"General","columns":2,"controls":[
      {"name":"n","label":"&Number:","kind":"number","value":"1","group":"Numbers"},
      {"name":"fixture.echo","label":"","kind":"label","value":"","group":"Numbers"},
      {"name":"fixture.tasks","label":"Tasks","kind":"grid","columns":[{"name":"Name","kind":"text"}],"value":[],"min_rows":3},
      {"name":"fixture.sample","label":"Sample","kind":"sample","value":{"text":"AaBbYyZz"}},
      {"name":"fixture.memo","label":"&Memo:","kind":"multiline","value":"","span":2,"fill":true}]}],
     "buttons":["OK","Cancel","Help",{"id":"bump","label":"&Bump"}]},
    {"id":"fixture.sheet","title":"Fixture Sheet","modeless":true,"placement":"right","pages":[{"name":"Sheet","controls":[
      {"name":"fixture.rows","label":"Tasks","kind":"grid","commit_command":true,"min_rows":6,"columns":[{"name":"Pick","kind":"check"},{"name":"Name","kind":"text"},{"name":"Units","kind":"number"}],"value":[]}]}],
     "buttons":[{"id":"apply","label":"&Apply","default":true},{"id":"clear","label":"Clear"},{"id":"close","label":"Close","cancel":true}]}],
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
    req.get("view")
        .and_then(|v| v.get("switches"))
        .and_then(|s| s.get("fixture.flag"))
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
        r#"{{"controls":{{"fixture.flag":{{"enabled":true,"checked":{}}},"fixture.bold":{{"enabled":{selected},"checked":{}}},"fixture.rename":{{"enabled":{selected}}}}},"gallery":{{"fixture.palette":{}}}"#,
        flag(&req),
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
            reply.push_str(&format!(
                r#","values":{{"fixture.echo":{},"fixture.tasks":[{}],"fixture.tasks.selected_row":{selected},"fixture.sample":{{"text":"AaBbYyZz","bold":{},"size":{}}}}}"#,
                quote(&format!("Number {n} for {name}")),
                rows.join(","),
                n_val as i64 % 2 == 1,
                12 + (n_val as i64).clamp(0, 12)
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
        "fixture.format" => r#"{"ops":[],"status":"Fixture: format"}"#.to_string(),
        "fixture.hello" => match (req.get("button").and_then(Json::str), req.get("values")) {
            // Help (#1609): a sentence on the status line, the dialog kept open.
            (Some("help"), _) => r#"{"ops":[],"status":"Fixture: help"}"#.to_string(),
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
                            format!(r#"{{"uid":{uid},"column":{},"bold":{on}}}"#, quote(col))
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
        "fixture.flag" => {
            let on = !flag(&req);
            format!(
                r#"{{"ops":[],"status":"Fixture: flag {}","view":{{"switches":{{"fixture.flag":{on}}}}}}}"#,
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
    let reply = format!(r#"{{"shapes":[{}]}}"#, shapes.join(","));
    // SAFETY: the ABI's contract.
    unsafe { give(out, reply.into_bytes()) };
    OK
}
