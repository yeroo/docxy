//! Every editable input outside the dialogs (#1029), so none is forgotten.
//!
//! The dialogs' fields are in [`crate::dialog::catalog`] and typed into by
//! the `inputs-typing-*.uit` cases. These are the rest: each has the target
//! id `pointer-click {"input":id}` takes, and the issue whose slice of
//! #1029 types into it. Until that slice lands, `pointer-click` names it.
//! The sheet's inputs are covered (#1200): `pointer-click` presses them where
//! they are drawn and `field-read {"input":id}` reads them, for
//! `uiharness/cases/sheet-inputs-typing.uit`.
//! Unlike the dialog catalogue, nothing finds a new input added outside
//! this list: it is kept by review (`qa/inputs-typing.md`). The sheet's Name
//! Box is display-only today (#1200 lists what it lacks), the suite has no
//! Styles gallery search or tab rename, and Settings' only typed values are
//! in the User name dialog, which the catalogue lists.

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
    }
}

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
        "the Find bar / Find & Replace (document)",
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
        "save-as-name",
        "Save As's file name",
        Surface::Backstage,
        BACKSTAGE,
    ),
    input(
        "info-title",
        "Info's Title property",
        Surface::Backstage,
        BACKSTAGE,
    ),
    input(
        "info-tags",
        "Info's Tags property",
        Surface::Backstage,
        BACKSTAGE,
    ),
    input(
        "info-author",
        "Info's Author property",
        Surface::Backstage,
        BACKSTAGE,
    ),
    input(
        "project-cell",
        "the Project grid's inline cell editor",
        Surface::Project,
        PROJECT,
    ),
    input(
        "task-info",
        "Task Information's fields",
        Surface::Project,
        PROJECT,
    ),
    input(
        "docxy-prompt",
        "the docxy terminal editor's prompts",
        Surface::Terminal,
        TERMINAL,
    ),
    input(
        "xlsxy-prompt",
        "the xlsxy terminal editor's prompts",
        Surface::Terminal,
        TERMINAL,
    ),
    input(
        "yppxy-prompt",
        "the yppxy terminal editor's prompts",
        Surface::Terminal,
        TERMINAL,
    ),
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
    /// A double-click: the in-cell editor opens on one, as it does for a
    /// person.
    pub double: bool,
}

/// The registered input `id`, or an error listing them all.
fn find(id: &str) -> Result<&'static Input, String> {
    registry().iter().find(|i| i.id == id).ok_or_else(|| {
        let ids: Vec<&str> = registry().iter().map(|i| i.id).collect();
        format!("unknown input '{id}'; inputs: {}", ids.join(", "))
    })
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
        _ => None,
    }
}

/// Where `pointer-click {"input":id}` presses, from the frame on screen.
/// The in-cell editor is a double-click on the active cell, which opens it;
/// it is refused while an entry is open, where a click on the cell would
/// commit it. Inputs of other surfaces answer which issue makes them
/// targetable.
pub(crate) fn press(app: &crate::Docxy, id: &str) -> Result<Press, String> {
    let i = find(id)?;
    if i.surface != Surface::Sheet {
        return Err(not_covered(i));
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
        double: false,
    })
}

/// `field-read {"input":id}`: a sheet input as it is edited and drawn, in the
/// shape of a dialog field's (`value`, `focused`, `caret` and `selection` in
/// chars, `null` while it has no focus). The formula bar and the in-cell
/// editor are one buffer, so both are focused while a cell is being edited:
/// nothing records which one the edit began in. The Find bar's fields append
/// and pop only, so a focused one's caret is its end. The Name Box and the
/// Font combos never take the focus today: they read what they draw.
pub(crate) fn read(app: &crate::Docxy, id: &str) -> Result<Json, String> {
    let i = find(id)?;
    if i.surface != Surface::Sheet {
        return Err(not_covered(i));
    }
    let v = crate::harness::sheet(app)?;
    let num = |n: usize| Json::Num(n as f64);
    let (value, focused, caret, selection) = match id {
        "formula-bar" | "cell-editor" => match &v.editing {
            Some(buf) => {
                let len = buf.chars().count();
                let sel = v
                    .edit_proposal
                    .as_ref()
                    .map(|p| Json::Arr(vec![num(p.0), num(len)]));
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
                num(text.chars().count())
            } else {
                Json::Null
            };
            (Json::Str(text.clone()), focused, caret, Json::Null)
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
        _ => return Err(not_covered(i)),
    };
    Ok(Json::obj(vec![
        ("input", Json::Str(id.into())),
        ("value", value),
        ("focused", Json::Bool(focused)),
        ("caret", caret),
        ("selection", selection),
    ]))
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
            let want = match i.surface {
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
                "sheet-font-size"
            ]
        );
        for id in sheet {
            assert_eq!(sheet_probe(id).is_none(), id == "cell-editor", "{id}");
        }
    }

    /// The Font combos read the value the ribbon table draws.
    #[test]
    fn the_font_combos_read_their_drawn_values() {
        assert_eq!(combo_value("font-name"), Some("Calibri"));
        assert_eq!(combo_value("font-size"), Some("11"));
        assert_eq!(combo_value("bold"), None);
    }
}
