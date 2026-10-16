//! Every editable input outside the dialogs (#1029), so none is forgotten.
//!
//! The dialogs' fields are in [`crate::dialog::catalog`] and typed into by
//! the `inputs-typing-*.uit` cases. These are the rest: each has the target
//! id `pointer-click {"input":id}` takes, and the issue whose slice of
//! #1029 types into it. Until that slice lands, `pointer-click` names it.
//! The sheet's inputs are covered (#1200): `pointer-click` presses them where
//! they are drawn and `field-read {"input":id}` reads them, for
//! `uiharness/cases/sheet-inputs-typing.uit`. So are the document's
//! (#1201): the Find bar's Find and Replace fields, Home's Font and Font
//! Size boxes and the comment editor, for
//! `uiharness/cases/doc-inputs-typing.uit`. So is the sheet tab's inline
//! rename (#1202), for `uiharness/cases/backstage-inputs-typing.uit`, which
//! also pins why Backstage's are not typed into: Save As's file name is the
//! OS's file dialog, and File > Info has no properties yet (#1364). So are
//! Project's (#1203): the grid's inline cell editor and Task >
//! Information...'s constraint prompt, for
//! `uiharness/cases/project-inputs-typing.uit`; the suite has no Task
//! Information dialog yet (#1368). The terminal editors' prompts (#1204)
//! are listed too, one id per field: docxy, xlsxy and yppxy are other
//! programs, so the suite's verbs only name the case that types into each,
//! `uiharness/cases/terminal-<app>-inputs-typing.uit`, which runs the editor
//! in a pseudo-terminal and reads the field with the editor's own
//! `field-read`; a test here fails when an id is missing from its case.
//! Unlike the dialog catalogue, nothing finds a new input added outside
//! this list: it is kept by review (`qa/inputs-typing.md`). The sheet's Name
//! Box is display-only today (#1200 lists what it lacks), the suite has no
//! Styles gallery search and no document tab or title-bar rename, and
//! Settings' only typed values are in the User name and Custom Lists
//! dialogs, which the catalogue lists.

use ctlcore::json::Json;
use gpui::{Pixels, Point};

/// Where an input lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    Doc,
    Sheet,
    Project,
    Backstage,
    Terminal,
}

impl Surface {
    fn name(self) -> &'static str {
        match self {
            Self::Doc => "document",
            Self::Sheet => "sheet",
            Self::Project => "Project",
            Self::Backstage => "Backstage",
            Self::Terminal => "terminal",
        }
    }
}

/// Whether the harness can reach an input at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// Drawn by the suite, where `pointer-click` can press it.
    Harness,
    /// The OS draws it, in a dialog a harness instance never opens: why.
    Native(&'static str),
    /// The suite does not have it yet: the issue that adds it.
    Missing(u32),
}

/// One input.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Input {
    /// The `pointer-click {"input":id}` target.
    pub id: &'static str,
    /// What a person calls it.
    pub name: &'static str,
    pub surface: Surface,
    /// The issue that types into it with real keys.
    pub covered_by: u32,
    pub reach: Reach,
}

const SHEET: u32 = 1200;
const DOC: u32 = 1201;
const BACKSTAGE: u32 = 1202;
const PROJECT: u32 = 1203;
const TERMINAL: u32 = 1204;

const fn input(id: &'static str, name: &'static str, surface: Surface, covered_by: u32) -> Input {
    Input {
        id,
        name,
        surface,
        covered_by,
        reach: Reach::Harness,
    }
}

/// A terminal editor's prompt (#1204).
const fn terminal(id: &'static str, name: &'static str) -> Input {
    input(id, name, Surface::Terminal, TERMINAL)
}

/// What the suite's `pointer-click` and `field-read` answer for a terminal
/// editor's input: the editor is another program, typed into in a
/// pseudo-terminal by its own case, which reads it with the editor's own
/// `field-read`.
fn terminal_case(i: &Input) -> String {
    let (app, file) = terminal_case_file(i.id);
    format!(
        "input '{}' ({}) is in the {app} terminal editor, not the suite: \
         uiharness/cases/{file} types into it in a pseudo-terminal and reads \
         it with {app}'s own field-read",
        i.id, i.name
    )
}

/// The editor a terminal input is in, and the case that types into it: the
/// vim `:` lines need `--vim`, so they have cases of their own.
fn terminal_case_file(id: &str) -> (&str, String) {
    let app = id.split('-').next().unwrap_or_default();
    let vim = if id.ends_with("-vim-command") {
        "-vim"
    } else {
        ""
    };
    (app, format!("terminal-{app}{vim}-inputs-typing.uit"))
}

/// A Backstage input the harness cannot reach, and why.
const fn unreachable_input(id: &'static str, name: &'static str, reach: Reach) -> Input {
    Input {
        reach,
        ..input(id, name, Surface::Backstage, BACKSTAGE)
    }
}

/// Backstage > Save As, F12, Export > Change File Type and the first Save of
/// a new document all open `rfd`'s file dialog, which a harness instance
/// refuses: its modal loop stops the control pipe.
const SAVE_AS_NATIVE: &str = "Save As's file name is the OS's file dialog, \
     which a harness instance never opens; see qa/inputs-typing.md";

/// The issue that adds File > Info's properties.
const INFO_PROPERTIES: u32 = 1364;

static REGISTRY: &[Input] = &[
    input("formula-bar", "the formula bar", Surface::Sheet, SHEET),
    input("cell-editor", "the in-cell editor", Surface::Sheet, SHEET),
    input("name-box", "the Name Box", Surface::Sheet, SHEET),
    input(
        "sheet-find",
        "the Find bar's Find field (sheet)",
        Surface::Sheet,
        SHEET,
    ),
    input(
        "sheet-replace",
        "the Find bar's Replace field (sheet)",
        Surface::Sheet,
        SHEET,
    ),
    input(
        "sheet-font",
        "the ribbon's Font combo (sheet)",
        Surface::Sheet,
        SHEET,
    ),
    input(
        "sheet-font-size",
        "the ribbon's Font Size combo (sheet)",
        Surface::Sheet,
        SHEET,
    ),
    input(
        "doc-find",
        "the Find bar's Find field (document)",
        Surface::Doc,
        DOC,
    ),
    input(
        "doc-replace",
        "the Find bar's Replace field (document)",
        Surface::Doc,
        DOC,
    ),
    input(
        "doc-font",
        "the ribbon's Font combo (document)",
        Surface::Doc,
        DOC,
    ),
    input(
        "doc-font-size",
        "the ribbon's Font Size combo (document)",
        Surface::Doc,
        DOC,
    ),
    input("comment-editor", "the comment editor", Surface::Doc, DOC),
    input(
        "sheet-tab-rename",
        "the sheet tab's inline rename",
        Surface::Sheet,
        BACKSTAGE,
    ),
    unreachable_input(
        "save-as-name",
        "Save As's file name",
        Reach::Native(SAVE_AS_NATIVE),
    ),
    unreachable_input(
        "info-title",
        "Info's Title property",
        Reach::Missing(INFO_PROPERTIES),
    ),
    unreachable_input(
        "info-tags",
        "Info's Tags property",
        Reach::Missing(INFO_PROPERTIES),
    ),
    unreachable_input(
        "info-author",
        "Info's Author property",
        Reach::Missing(INFO_PROPERTIES),
    ),
    input(
        "project-cell",
        "the Project grid's inline cell editor",
        Surface::Project,
        PROJECT,
    ),
    // Task > Information... opens the prompt bar's Constraint prompt: the
    // suite has no Task Information dialog yet (#1368). The bar's other
    // prompts (Notes, Hyperlink, Assign, Move, Predecessor, Find) are the
    // same field on the same key path, so they are not inputs of their own.
    input(
        "task-info",
        "Task > Information...'s constraint prompt",
        Surface::Project,
        PROJECT,
    ),
    // The terminal editors' prompts (#1204): typed into through a
    // pseudo-terminal by uiharness/cases/terminal-<app>-inputs-typing.uit,
    // read by each editor's own `field-read`. A prompt several commands
    // share is one input: xlsxy's minibuffer (Rename Sheet, Find, Go To,
    // F12's Save As, ...) and yppxy's prompt (Rename, Duration, Find, its
    // vim `:` line, ...).
    terminal("docxy-find", "docxy's Find bar: Find field"),
    terminal("docxy-replace", "docxy's Find bar: Replace field"),
    terminal("docxy-comment", "docxy's new comment text"),
    terminal("docxy-compare-original", "docxy's Compare: original path"),
    terminal("docxy-compare-revised", "docxy's Compare: revised path"),
    terminal("docxy-vim-command", "docxy's vim : command line"),
    terminal("docxy-save-as", "docxy's Backstage Save As file name"),
    terminal("xlsxy-prompt", "xlsxy's minibuffer prompt"),
    terminal("xlsxy-cell", "xlsxy's cell editor"),
    terminal("xlsxy-vim-command", "xlsxy's vim : command line"),
    terminal("xlsxy-save-as", "xlsxy's Backstage Save As file name"),
    terminal("yppxy-prompt", "yppxy's prompt"),
    terminal("yppxy-save-as", "yppxy's Backstage Save As file name"),
];

/// The registry.
pub(crate) fn registry() -> &'static [Input] {
    REGISTRY
}

/// How `pointer-click {"input":id}` presses an input: where, and with how
/// many clicks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Press {
    pub at: Point<Pixels>,
    /// A double-click: the in-cell editor and the sheet tab's rename open on
    /// one, as they do for a person.
    pub double: bool,
}

/// The registered input `id`, or an error listing them all.
fn find(id: &str) -> Result<&'static Input, String> {
    registry().iter().find(|i| i.id == id).ok_or_else(|| {
        let ids: Vec<&str> = registry().iter().map(|i| i.id).collect();
        format!("unknown input '{id}'; inputs: {}", ids.join(", "))
    })
}

/// Why the harness cannot reach `i`, if it cannot: the OS draws it, or the
/// suite does not have it yet.
fn unreachable(i: &Input) -> Option<String> {
    match i.reach {
        Reach::Harness => None,
        Reach::Native(why) => Some(why.into()),
        Reach::Missing(issue) => Some(format!(
            "File > Info has no {} yet, see #{issue}",
            i.name.trim_start_matches("Info's ")
        )),
    }
}

/// The answer for an input whose slice has not landed.
fn not_covered(i: &Input) -> String {
    format!(
        "input '{}' ({}, {}) is not covered yet, see #{}",
        i.id,
        i.name,
        i.surface.name(),
        i.covered_by
    )
}

/// The probe a sheet input is drawn under (`None`: the in-cell editor,
/// which is the active cell itself).
fn sheet_probe(id: &str) -> Option<&'static str> {
    match id {
        "formula-bar" => Some("fx-edit"),
        "name-box" => Some("name-box"),
        "sheet-find" => Some("sf-q"),
        "sheet-replace" => Some("sf-r"),
        "sheet-font" => Some("sheet-combo:font-name"),
        "sheet-font-size" => Some("sheet-combo:font-size"),
        "sheet-tab-rename" => Some("sheet-tab-name"),
        _ => None,
    }
}

/// The probe a document input is drawn under.
fn doc_probe(id: &str) -> Option<&'static str> {
    match id {
        "doc-find" => Some("df-q"),
        "doc-replace" => Some("df-r"),
        "doc-font" => Some("doc-combo:fontname"),
        "doc-font-size" => Some("doc-combo:fontsize"),
        "comment-editor" => Some("comment-edit"),
        _ => None,
    }
}

/// Where `pointer-click {"input":id}` presses, from the frame on screen.
/// A sheet input is pressed here: the in-cell editor is a double-click on
/// the active cell, which opens it, refused while an entry is open, where a
/// click on the cell would commit it; the sheet tab's rename is a
/// double-click on the active tab's name, refused while a rename is open. A
/// document input goes to [`press_doc`]. A Backstage input says why the
/// harness cannot reach it; a Project input goes to [`press_project`].
/// A terminal editor's input names the case that types into it.
pub(crate) fn press(app: &crate::Docxy, id: &str) -> Result<Press, String> {
    let i = find(id)?;
    if let Some(why) = unreachable(i) {
        return Err(why);
    }
    match i.surface {
        Surface::Sheet => {}
        Surface::Doc => return press_doc(app, i),
        Surface::Project => return press_project(app, i),
        Surface::Terminal => return Err(terminal_case(i)),
        _ => return Err(not_covered(i)),
    }
    let v = crate::harness::sheet(app)?;
    let Some(probe) = sheet_probe(id) else {
        if v.editing.is_some() {
            return Err(
                "the in-cell editor is already open; a click on its cell commits it".into(),
            );
        }
        let b = app.cells_bounds(v.sel, v.sel)?;
        return Ok(Press {
            at: b.center(),
            double: true,
        });
    };
    // A double-click opens the rename, as it does for a person.
    let rename = id == "sheet-tab-rename";
    if rename && app.sheet_rename.is_some() {
        return Err("the sheet tab's rename is already open".into());
    }
    let bounds = app.probes.borrow().on_screen(probe);
    let b = bounds.ok_or_else(|| match id {
        "sheet-find" | "sheet-replace" => {
            format!("{} is not drawn: open the Find bar first (Ctrl+F)", i.name)
        }
        "sheet-font" | "sheet-font-size" => {
            format!("{} is not drawn: show the Home ribbon tab first", i.name)
        }
        _ => format!("{} is not drawn", i.name),
    })?;
    Ok(Press {
        at: b.center(),
        double: rename,
    })
}

/// A document input, pressed where it is drawn.
fn press_doc(app: &crate::Docxy, i: &Input) -> Result<Press, String> {
    crate::harness::active_doc(app)?;
    let probe = doc_probe(i.id).ok_or_else(|| not_covered(i))?;
    let bounds = app.probes.borrow().on_screen(probe);
    let b = bounds.ok_or_else(|| match i.id {
        "doc-find" | "doc-replace" => {
            format!("{} is not drawn: open the Find bar first (Ctrl+F)", i.name)
        }
        "doc-font" | "doc-font-size" => {
            format!("{} is not drawn: show the Home ribbon tab first", i.name)
        }
        "comment-editor" => format!(
            "{} is not drawn: select text, then Review > New Comment",
            i.name
        ),
        _ => format!("{} is not drawn", i.name),
    })?;
    Ok(Press {
        at: b.center(),
        double: false,
    })
}

/// `field-read {"input":id}`: a sheet input as it is edited and drawn (a
/// document input goes to [`read_doc`] and a Project input to
/// [`read_project`]; a Backstage input says why the harness cannot reach
/// it, and a terminal editor's input names the case that reads it), in the
/// shape of a dialog
/// field's (`value`, `focused`, `caret` and `selection` in chars, `null`
/// while it has no focus). The formula bar and the in-cell
/// editor are one buffer, so both are focused while a cell is being edited:
/// nothing records which one the edit began in. The sheet Find bar's fields
/// keep a caret and a whole-field selection (#1342); the sheet tab's rename
/// appends and pops only, so its caret is its end, and it reads the active sheet's name while it is closed. The
/// Name Box and the Font combos never take the focus today: they read what
/// they draw.
pub(crate) fn read(app: &crate::Docxy, id: &str) -> Result<Json, String> {
    let i = find(id)?;
    if let Some(why) = unreachable(i) {
        return Err(why);
    }
    match i.surface {
        Surface::Sheet => {}
        Surface::Doc => return read_doc(app, i),
        Surface::Project => return read_project(app, i),
        Surface::Terminal => return Err(terminal_case(i)),
        _ => return Err(not_covered(i)),
    }
    let v = crate::harness::sheet(app)?;
    let num = |n: usize| Json::Num(n as f64);
    let (value, focused, caret, selection) = match id {
        "formula-bar" | "cell-editor" => match &v.editing {
            Some(buf) => {
                let len = buf.chars().count();
                let sel = if v.edit_select_all {
                    Some(Json::Arr(vec![num(0), num(len)]))
                } else {
                    v.edit_proposal
                        .as_ref()
                        .map(|p| Json::Arr(vec![num(p.0), num(len)]))
                };
                (
                    Json::Str(buf.clone()),
                    true,
                    num(v.edit_caret.min(len)),
                    sel.unwrap_or(Json::Null),
                )
            }
            // Idle, the bar shows the active cell's formula or text; the
            // cell editor has none.
            None if id == "formula-bar" => (
                Json::Str(crate::fx_bar_text(v)),
                false,
                Json::Null,
                Json::Null,
            ),
            None => (Json::Null, false, Json::Null, Json::Null),
        },
        "sheet-find" | "sheet-replace" => {
            let (text, field) = if id == "sheet-find" {
                (&app.find_query, crate::FindField::Query)
            } else {
                (&app.replace_text, crate::FindField::Replace)
            };
            let focused = app.find_open && app.find_field == field;
            let caret = if focused {
                num(app.sf_caret_of(field))
            } else {
                Json::Null
            };
            let len = text.chars().count();
            let selection = if focused && app.sf_sel[field.idx()] && len > 0 {
                Json::Arr(vec![num(0), num(len)])
            } else {
                Json::Null
            };
            (Json::Str(text.clone()), focused, caret, selection)
        }
        "name-box" => (
            Json::Str(crate::name_box_text(v)),
            false,
            Json::Null,
            Json::Null,
        ),
        "sheet-font" | "sheet-font-size" => {
            let cmd = if id == "sheet-font" {
                "font-name"
            } else {
                "font-size"
            };
            let value = combo_value(cmd).ok_or_else(|| format!("no combo '{cmd}'"))?;
            (Json::Str(value.into()), false, Json::Null, Json::Null)
        }
        "sheet-tab-rename" => match &app.sheet_rename {
            Some((_, buf)) => (
                Json::Str(buf.clone()),
                true,
                num(buf.chars().count()),
                Json::Null,
            ),
            None => {
                let name = v.pkg.workbook.sheets.get(v.active).map(|s| s.name.clone());
                (
                    name.map_or(Json::Null, Json::Str),
                    false,
                    Json::Null,
                    Json::Null,
                )
            }
        },
        _ => return Err(not_covered(i)),
    };
    Ok(field_json(id, value, focused, caret, selection))
}

/// `field-read {"input":id}` for a document input, in the sheet inputs'
/// shape. The Find bar's fields and the comment editor append and pop only,
/// so a focused one's caret is its end; the Find bar is the window's, shared
/// with the sheet's, so it reads only while a document is active. Ctrl+F
/// opens the Find bar under an open comment bar, but `on_key` gives the
/// comment bar the keys first, so a Find field is focused only while no
/// comment is being typed. The Font boxes never take the focus today: they
/// read what they draw.
fn read_doc(app: &crate::Docxy, i: &Input) -> Result<Json, String> {
    crate::harness::active_doc(app)?;
    let end_if = |text: &str, focused: bool| {
        if focused {
            Json::Num(text.chars().count() as f64)
        } else {
            Json::Null
        }
    };
    let (value, focused, caret) = match i.id {
        "doc-find" | "doc-replace" => {
            let (text, field) = if i.id == "doc-find" {
                (&app.find_query, crate::FindField::Query)
            } else {
                (&app.replace_text, crate::FindField::Replace)
            };
            let focused = app.find_open && !app.comment_open && app.find_field == field;
            (Json::Str(text.clone()), focused, end_if(text, focused))
        }
        "doc-font" | "doc-font-size" => {
            let cmd = if i.id == "doc-font" {
                "fontname"
            } else {
                "fontsize"
            };
            let value = crate::doc_combo_value(cmd, app.caret_run_props());
            (Json::Str(value), false, Json::Null)
        }
        "comment-editor" => {
            let focused = app.comment_open;
            (
                Json::Str(app.comment_text.clone()),
                focused,
                end_if(&app.comment_text, focused),
            )
        }
        _ => return Err(not_covered(i)),
    };
    Ok(field_json(i.id, value, focused, caret, Json::Null))
}

/// A Project input, pressed where it is drawn. The cell editor is a
/// double-click on the cursor's cell, which opens it as it does for a
/// person, resolved as `click C<n>` resolves it (clipped to the table's
/// viewport); it is refused while a cell edit is open, where a click commits
/// it, and while a prompt is open, where `open_cell` opens nothing. The
/// constraint prompt is a click on its field, refused while it is closed
/// and while another prompt holds the bar.
fn press_project(app: &crate::Docxy, i: &Input) -> Result<Press, String> {
    let v = crate::harness::project(app)?;
    let double = project_double(i.id).ok_or_else(|| not_covered(i))?;
    let b = match i.id {
        "project-cell" => {
            if v.cell.is_some() {
                return Err(
                    "a Project cell edit is already open; a click on a cell commits it".into(),
                );
            }
            if let Some(p) = &v.prompt {
                return Err(format!(
                    "the {} prompt is open; a double-click opens no cell under it",
                    p.kind.name()
                ));
            }
            let (r, c) = (v.display_row() as u32, v.col as u32);
            app.project_region_bounds(
                crate::harness::Region::Cells(r, c, r, c),
                crate::Probes::on_screen,
            )?
        }
        "task-info" => {
            match &v.prompt {
                Some(p) if p.kind == crate::PromptKind::Constraint => {}
                Some(p) => {
                    return Err(format!(
                        "the open prompt is {}, not Task > Information...'s constraint",
                        p.kind.name()
                    ));
                }
                None => {
                    return Err(format!(
                        "{} is not drawn: Task > Information... first",
                        i.name
                    ));
                }
            }
            let bounds = app.probes.borrow().on_screen("project-prompt-field");
            bounds.ok_or_else(|| format!("{} is not drawn", i.name))?
        }
        _ => return Err(not_covered(i)),
    };
    Ok(Press {
        at: b.center(),
        double,
    })
}

/// Whether `pointer-click` double-clicks a Project input: the cell editor
/// opens on a double-click, the prompt's field takes one click.
fn project_double(id: &str) -> Option<bool> {
    match id {
        "project-cell" => Some(true),
        "task-info" => Some(false),
        _ => None,
    }
}

/// `field-read {"input":id}` for a Project input, in the sheet inputs'
/// shape. The cell editor's caret is a byte offset, read in chars; it has
/// no selection. Idle, it reads the text the cursor's cell would open with
/// (none on the entry row). The constraint prompt appends and pops only, so
/// a focused one's caret is its end; another prompt kind is not it.
fn read_project(app: &crate::Docxy, i: &Input) -> Result<Json, String> {
    let v = crate::harness::project(app)?;
    let (value, focused, caret) = match i.id {
        "project-cell" => match &v.cell {
            Some(c) => (
                Json::Str(c.buf.clone()),
                true,
                Json::Num(char_caret(&c.buf, c.caret) as f64),
            ),
            None => {
                let text =
                    v.ed.project()
                        .tasks
                        .get(v.cursor_row())
                        .map(|t| crate::cell_edit_text(&v.ed, t, v.col));
                (text.map_or(Json::Null, Json::Str), false, Json::Null)
            }
        },
        "task-info" => match &v.prompt {
            Some(p) if p.kind == crate::PromptKind::Constraint => (
                Json::Str(p.buf.clone()),
                true,
                Json::Num(p.buf.chars().count() as f64),
            ),
            _ => (Json::Null, false, Json::Null),
        },
        _ => return Err(not_covered(i)),
    };
    Ok(field_json(i.id, value, focused, caret, Json::Null))
}

/// A byte offset into `buf` as the chars before it, the unit `field-read`
/// reports.
fn char_caret(buf: &str, byte: usize) -> usize {
    buf.char_indices().take_while(|&(i, _)| i < byte).count()
}

/// The reply both surfaces' `field-read {"input":id}` give.
fn field_json(id: &str, value: Json, focused: bool, caret: Json, selection: Json) -> Json {
    Json::obj(vec![
        ("input", Json::Str(id.into())),
        ("value", value),
        ("focused", Json::Bool(focused)),
        ("caret", caret),
        ("selection", selection),
    ])
}

/// The value a sheet ribbon combo draws.
fn combo_value(id: &str) -> Option<&'static str> {
    crate::sheet_ribbon::SHEET_RIBBON
        .iter()
        .flat_map(crate::sheet_ribbon::Tab::commands)
        .find(|c| c.id == id)
        .and_then(|c| match c.shape {
            crate::sheet_ribbon::Shape::Combo { value, .. } => Some(value),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1029: ids are unique, and each input names the slice that covers it.
    #[test]
    fn every_input_is_unique_and_owned_by_a_slice() {
        let mut ids: Vec<&str> = registry().iter().map(|i| i.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), registry().len(), "an input id repeats");
        for i in registry() {
            assert!(
                (SHEET..=TERMINAL).contains(&i.covered_by),
                "{} is covered by #{}, not one of #1200-#1204",
                i.id,
                i.covered_by
            );
            // A rename is #1202's, wherever it is drawn.
            let want = match i.surface {
                _ if i.id == "sheet-tab-rename" => BACKSTAGE,
                Surface::Sheet => SHEET,
                Surface::Doc => DOC,
                Surface::Backstage => BACKSTAGE,
                Surface::Project => PROJECT,
                Surface::Terminal => TERMINAL,
            };
            assert_eq!(i.covered_by, want, "{}: its surface's slice", i.id);
        }
    }

    /// #1200: every sheet input is pressed where it is drawn: each has a
    /// probe but the in-cell editor, which is the active cell.
    #[test]
    fn every_sheet_input_has_a_target() {
        let sheet: Vec<&str> = registry()
            .iter()
            .filter(|i| i.surface == Surface::Sheet)
            .map(|i| i.id)
            .collect();
        assert_eq!(
            sheet,
            [
                "formula-bar",
                "cell-editor",
                "name-box",
                "sheet-find",
                "sheet-replace",
                "sheet-font",
                "sheet-font-size",
                "sheet-tab-rename"
            ]
        );
        for id in sheet {
            assert_eq!(sheet_probe(id).is_none(), id == "cell-editor", "{id}");
        }
    }

    /// #1202: Backstage's inputs say why the harness cannot type into them,
    /// and every other input is one it can reach.
    #[test]
    fn unreachable_inputs_say_why() {
        let why = |id: &str| unreachable(find(id).unwrap());
        assert_eq!(
            why("save-as-name").as_deref(),
            Some(
                "Save As's file name is the OS's file dialog, which a harness \
                 instance never opens; see qa/inputs-typing.md"
            )
        );
        assert_eq!(
            why("info-title").as_deref(),
            Some("File > Info has no Title property yet, see #1364")
        );
        assert_eq!(
            why("info-tags").as_deref(),
            Some("File > Info has no Tags property yet, see #1364")
        );
        assert_eq!(
            why("info-author").as_deref(),
            Some("File > Info has no Author property yet, see #1364")
        );
        for i in registry() {
            assert_eq!(
                why(i.id).is_some(),
                i.surface == Surface::Backstage,
                "{}",
                i.id
            );
        }
    }

    /// #1201: every document input is pressed where it is drawn.
    #[test]
    fn every_doc_input_has_a_target() {
        let doc: Vec<&str> = registry()
            .iter()
            .filter(|i| i.surface == Surface::Doc)
            .map(|i| i.id)
            .collect();
        assert_eq!(
            doc,
            [
                "doc-find",
                "doc-replace",
                "doc-font",
                "doc-font-size",
                "comment-editor"
            ]
        );
        for id in doc {
            assert!(doc_probe(id).is_some(), "{id}");
        }
    }

    /// #1203: both Project inputs are pressed where they are drawn: the
    /// cursor's cell and the prompt bar's field.
    #[test]
    fn every_project_input_has_a_target() {
        let project: Vec<&str> = registry()
            .iter()
            .filter(|i| i.surface == Surface::Project)
            .map(|i| i.id)
            .collect();
        assert_eq!(project, ["project-cell", "task-info"]);
        for id in project {
            assert_eq!(project_double(id), Some(id == "project-cell"), "{id}");
        }
    }

    /// The Project cell editor's caret is a byte offset; `field-read`
    /// reports chars, as every other input does.
    #[test]
    fn the_project_cell_caret_reads_in_chars() {
        assert_eq!(char_caret("€1", "€1".len()), 2);
        assert_eq!(char_caret("€1", "€".len()), 1);
        assert_eq!(char_caret("€1", 0), 0);
        assert_eq!(char_caret("Ab1 ", 4), 4);
        assert_eq!(char_caret("ab", 9), 2);
    }

    /// The document Font boxes read what `combo_box` draws: the caret run's
    /// family and size, else Calibri 11.
    #[test]
    fn the_doc_font_boxes_read_their_drawn_values() {
        use docxcore::model::RunProps;
        assert_eq!(crate::doc_combo_value("fontname", None), "Calibri");
        assert_eq!(crate::doc_combo_value("fontsize", None), "11");
        let props = RunProps {
            font: Some("Aptos".into()),
            size_half_pts: Some(21),
            ..RunProps::default()
        };
        assert_eq!(
            crate::doc_combo_value("fontname", Some(props.clone())),
            "Aptos"
        );
        assert_eq!(crate::doc_combo_value("fontsize", Some(props)), "10.5");
        let whole = RunProps {
            size_half_pts: Some(24),
            ..RunProps::default()
        };
        assert_eq!(crate::doc_combo_value("fontsize", Some(whole)), "12");
    }

    /// The Font combos read the value the ribbon table draws.
    #[test]
    fn the_font_combos_read_their_drawn_values() {
        assert_eq!(combo_value("font-name"), Some("Calibri"));
        assert_eq!(combo_value("font-size"), Some("11"));
        assert_eq!(combo_value("bold"), None);
    }

    /// #1204: every terminal editor's input is typed into by its case, and
    /// the suite's verbs name that case.
    #[test]
    fn every_terminal_input_is_typed_into_by_its_case() {
        let cases = [
            (
                "terminal-docxy-inputs-typing.uit",
                include_str!("../../../uiharness/cases/terminal-docxy-inputs-typing.uit"),
            ),
            (
                "terminal-docxy-vim-inputs-typing.uit",
                include_str!("../../../uiharness/cases/terminal-docxy-vim-inputs-typing.uit"),
            ),
            (
                "terminal-xlsxy-inputs-typing.uit",
                include_str!("../../../uiharness/cases/terminal-xlsxy-inputs-typing.uit"),
            ),
            (
                "terminal-xlsxy-vim-inputs-typing.uit",
                include_str!("../../../uiharness/cases/terminal-xlsxy-vim-inputs-typing.uit"),
            ),
            (
                "terminal-yppxy-inputs-typing.uit",
                include_str!("../../../uiharness/cases/terminal-yppxy-inputs-typing.uit"),
            ),
        ];
        let terminal: Vec<&Input> = registry()
            .iter()
            .filter(|i| i.surface == Surface::Terminal)
            .collect();
        assert_eq!(terminal.len(), 13);
        for i in terminal {
            let (app, file) = terminal_case_file(i.id);
            let (_, case) = cases
                .iter()
                .find(|(f, _)| *f == file)
                .unwrap_or_else(|| panic!("{}: no case {file}", i.id));
            let vim = if file.contains("-vim-") { " --vim" } else { "" };
            assert_eq!(
                case.lines().next(),
                Some(&format!("terminal {app}{vim}")[..]),
                "{file} drives {app}"
            );
            assert!(
                case.contains(&format!("{{\"input\":\"{}\"}}", i.id)),
                "{}: {file} never reads it",
                i.id
            );
            assert!(
                case.contains(&format!("test {app} {}: ", i.id)),
                "{}: {file} has no test for it",
                i.id
            );
            assert!(terminal_case(i).contains(&file), "{}", terminal_case(i));
        }
    }
}
