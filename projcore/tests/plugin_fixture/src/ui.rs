//! The fixture's version target and user interface (the optional symbols
//! `projcore::plugin_abi` documents): one of everything the hosts render, so
//! their plumbing is covered end to end without any real plugin.
//!
//! - Ribbon: on the Task tab a group "Fixture" with a large button
//!   `fixture.hello`, a toggle `fixture.flag` and a gallery `fixture.palette`
//!   of two swatches (`fixture.blue`, `fixture.red`).
//! - Menus: a row-menu item `fixture.rename` after "Notes...".
//! - Dialogs: a Task Information page "Fixture" with a multiline and a grid,
//!   and a dialog `fixture.options` (one number) that `fixture.hello` opens;
//!   OK sends `fixture.hello` again with the values.
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
      {"id":"fixture.red","label":"Red","color":"#CC3333","preview":"fixture.red"}]}]}]}],
  "menus":[{"menu":"row","items":[{"id":"fixture.rename","label":"Rename (fixture)","icon":"comment","after":"Notes..."}]}],
  "dialogs":[
    {"id":"task-information","pages":[{"name":"Fixture","controls":[
      {"name":"fixture.note","label":"Fixture &note:","kind":"multiline","value":""},
      {"name":"fixture.grid","label":"Fixture items","kind":"grid","columns":[{"name":"Name","kind":"text"},{"name":"Done","kind":"checkbox"}],"value":[["a","false"],["b","true"]]}]}]},
    {"id":"fixture.options","title":"Fixture Options","pages":[{"name":"General","controls":[
      {"name":"n","label":"&Number:","kind":"number","value":"1"}]}],"buttons":["OK","Cancel"]}],
  "palettes":[
    {"id":"fixture.blue","label":"Blue","bars":[{"kind":"on-track","color":"#3366CC","shape":"bar"},{"kind":"critical","color":"#1F3763","shape":"bar"}]},
    {"id":"fixture.red","label":"Red","bars":[{"kind":"on-track","color":"#CC3333","shape":"bar"},{"kind":"critical","color":"#7F0000","shape":"bar"}]}],
  "text_style_categories":["Fixture Tasks"]
}"##;

/// The chosen palette, kept across calls.
static PALETTE: Mutex<String> = Mutex::new(String::new());

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
        r#"{{"controls":{{"fixture.flag":{{"enabled":true,"checked":{}}},"fixture.rename":{{"enabled":{selected}}}}},"gallery":{{"fixture.palette":{}}}"#,
        flag(&req),
        quote(&palette())
    );
    // A Task Information dialog about to open: the page's values.
    if let Some(dialog) = req.get("dialog") {
        if dialog.get("id").and_then(Json::str) == Some("task-information") {
            let uid = dialog.get("uid").and_then(Json::num).unwrap_or(0.0) as i64;
            reply.push_str(&format!(
                r#","values":{{"fixture.note":{},"fixture.grid":[["task","false"],["{uid}","true"]]}}"#,
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
        "fixture.hello" => match req.get("values") {
            None => {
                r#"{"ops":[],"dialog":"fixture.options","status":"Fixture: asking"}"#.to_string()
            }
            Some(values) => {
                let n = values.get("n").map_or_else(String::new, |n| match n {
                    Json::Str(s) => s.clone(),
                    Json::Num(x) => x.to_string(),
                    _ => String::new(),
                });
                format!(
                    r#"{{"ops":[],"status":{}}}"#,
                    quote(&format!("Fixture: hello {n}"))
                )
            }
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
            Some(uid) => format!(
                r#"{{"ops":[{{"verb":"task.set","args":{{"uid":{uid},"name":"Renamed by fixture"}}}}],"status":{}}}"#,
                quote(&format!("Fixture: renamed task {uid}"))
            ),
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
