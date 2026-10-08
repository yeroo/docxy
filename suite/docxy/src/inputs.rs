//! Every editable input outside the dialogs (#1029), so none is forgotten.
//!
//! The dialogs' fields are in [`crate::dialog::catalog`] and typed into by
//! the `inputs-typing-*.uit` cases. These are the rest: each has the target
//! id `pointer-click {"input":id}` will take, and the issue whose slice of
//! #1029 types into it. Until that slice lands, `pointer-click` names it.
//! Unlike the dialog catalogue, nothing finds a new input added outside
//! this list: it is kept by review (`qa/inputs-typing.md`). The suite has no
//! Name Box, Styles gallery search or tab rename today, and Settings' only
//! typed values are in the User name dialog, which the catalogue lists.

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
    input(
        "sheet-find",
        "Find & Replace (sheet)",
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

/// Where `pointer-click {"input":id}` clicks. No input is targetable yet:
/// each answers which issue makes it so.
pub(crate) fn point(_app: &crate::Docxy, id: &str) -> Result<Point<Pixels>, String> {
    match registry().iter().find(|i| i.id == id) {
        Some(i) => Err(format!(
            "input '{id}' ({}, {}) is not covered yet, see #{}",
            i.name,
            i.surface.name(),
            i.covered_by
        )),
        None => {
            let ids: Vec<&str> = registry().iter().map(|i| i.id).collect();
            Err(format!("unknown input '{id}'; inputs: {}", ids.join(", ")))
        }
    }
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
}
