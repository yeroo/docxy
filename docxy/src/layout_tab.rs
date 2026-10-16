//! The Layout ribbon tab (#1274): Margins, Orientation, Size, Columns, Breaks,
//! Line Numbers and Hyphenation pickers, as in the suite's Layout tab (#649).
//!
//! Section commands edit the sections the caret or selection touches through
//! the body editor's [`Editor::edit_section_setups`], even while a header or
//! footer is open (the body caret picks the section). Margins is one undo step.
//! Mirrored margins and Hyphenation write the package's `settings.xml` only:
//! terminal `docxy` has no settings sync, so they have no undo (#1273).
use super::*;
use docxcore::model::{BreakKind, ClearKind};
use docxcore::sect::{LineNumbering, LnRestart, Paper, SectionSetup, SectionStart};

pub(crate) const MARGIN_ITEMS: &[&str] = &["Normal", "Narrow", "Moderate", "Wide", "Mirrored"];
pub(crate) const ORIENTATION_ITEMS: &[&str] = &["Portrait", "Landscape"];
/// `Paper::ALL`'s labels, in its order (a test keeps them in step).
pub(crate) const PAPER_ITEMS: &[&str] = &[
    "Letter",
    "Legal",
    "Executive",
    "A4",
    "A5",
    "B5 (JIS)",
    "Tabloid",
];
pub(crate) const COLUMN_ITEMS: &[&str] = &["One", "Two", "Three", "Left", "Right"];
pub(crate) const BREAK_ITEMS: &[&str] = &[
    "Page",
    "Column",
    "Text Wrapping",
    "Next Page",
    "Continuous",
    "Even Page",
    "Odd Page",
];
pub(crate) const LINE_NUMBER_ITEMS: &[&str] = &[
    "None",
    "Continuous",
    "Restart Each Page",
    "Restart Each Section",
    "Suppress for Current Paragraph",
];
pub(crate) const HYPHEN_ITEMS: &[&str] = &["None", "Automatic"];

pub(crate) const SETTINGS_UNREADABLE: &str =
    "Could not change the setting: the document's settings part is unreadable";

/// (top, bottom, left or inside, right or outside), in twips.
fn margin_values(label: &str) -> Option<(i32, i32, i32, i32)> {
    Some(match label {
        "Normal" => (1440, 1440, 1440, 1440),
        "Narrow" => (720, 720, 720, 720),
        "Moderate" => (1440, 1440, 1080, 1080),
        "Wide" => (1440, 1440, 2880, 2880),
        "Mirrored" => (1440, 1440, 1800, 1440),
        _ => return None,
    })
}

fn section_start(label: &str) -> Option<SectionStart> {
    Some(match label {
        "Next Page" => SectionStart::NextPage,
        "Continuous" => SectionStart::Continuous,
        "Even Page" => SectionStart::EvenPage,
        "Odd Page" => SectionStart::OddPage,
        _ => return None,
    })
}

impl App {
    /// The picker row to start on: the caret section's current choice.
    pub(crate) fn layout_current(&self, kind: PickerKind) -> usize {
        let ed = self.body_editor();
        let sections = ed.sections();
        let Some(raw) = sections.get(ed.caret_section().min(sections.len().saturating_sub(1)))
        else {
            return 0;
        };
        let setup = SectionSetup::parse(raw);
        let m = setup.margins;
        let item = match kind {
            PickerKind::Margins => MARGIN_ITEMS.iter().position(|l| {
                let (t, b, left, r) = margin_values(l).unwrap_or_default();
                (*l == "Mirrored") == self.pkg.has_mirror_margins()
                    && (m.top, m.bottom, m.left, m.right) == (t, b, left, r)
            }),
            PickerKind::Orientation => Some(usize::from(setup.page.landscape)),
            PickerKind::PaperSize => Paper::matching(setup.page.w, setup.page.h)
                .and_then(|p| Paper::ALL.iter().position(|q| *q == p)),
            PickerKind::Columns => {
                let c = &setup.columns;
                if c.count() == 1 {
                    Some(0)
                } else if c.equal_width() && c.count() == 2 {
                    Some(1)
                } else if c.equal_width() && c.count() == 3 {
                    Some(2)
                } else if c.cols.len() == 2 && c.cols[0].w < c.cols[1].w {
                    Some(3)
                } else if c.cols.len() == 2 && c.cols[0].w > c.cols[1].w {
                    Some(4)
                } else {
                    None
                }
            }
            PickerKind::LineNumbers => Some(match setup.line_numbers {
                None => 0,
                Some(l) => match l.restart {
                    LnRestart::Continuous => 1,
                    LnRestart::NewPage => 2,
                    LnRestart::NewSection => 3,
                },
            }),
            PickerKind::Hyphenation => Some(usize::from(self.pkg.has_auto_hyphenation())),
            _ => None,
        };
        item.unwrap_or(0)
    }

    /// Open a Layout picker. Like the Design pickers it needs no selection and
    /// refuses a Markdown document.
    pub(crate) fn open_layout_picker(&mut self, kind: PickerKind) {
        if self.format == DocFormat::Markdown {
            self.status = Some(format!(
                "{} needs a .docx (not Markdown)",
                kind.title().trim()
            ));
            self.dirty = true;
            return;
        }
        let sel = self.layout_current(kind);
        self.font_picker = Some(FontPicker { kind, sel });
        self.dirty = true;
    }

    /// Apply a Layout pick. The status says what happened; an error says why
    /// nothing did. The document turns modified only when something changed.
    pub(crate) fn apply_layout_pick(&mut self, kind: PickerKind, item: &str) {
        match self.layout_pick(kind, item) {
            Ok((changed, status)) => {
                if changed {
                    self.sync_page_parts();
                    self.after_edit();
                }
                self.status = Some(status);
            }
            Err(e) => self.status = Some(e),
        }
        self.dirty = true;
    }

    fn layout_pick(&mut self, kind: PickerKind, item: &str) -> Result<(bool, String), String> {
        match kind {
            PickerKind::Margins => {
                let (top, bottom, left, right) =
                    margin_values(item).ok_or_else(|| format!("Unknown margins: {item}"))?;
                let mirror = item == "Mirrored";
                if self.pkg.has_mirror_margins() != mirror {
                    // The file first: a settings part docxy cannot edit
                    // refuses the command with nothing changed (#1273).
                    self.pkg.set_mirror_margins(mirror);
                    if self.pkg.has_mirror_margins() != mirror {
                        return Err(SETTINGS_UNREADABLE.into());
                    }
                    self.modified = true;
                }
                let ed = self.body_editor_mut();
                let k = ed.target_sections();
                let mut changed = false;
                ed.one_step("Margins", |ed| {
                    changed = ed.edit_section_setups(&k, |s| {
                        (
                            s.margins.top,
                            s.margins.bottom,
                            s.margins.left,
                            s.margins.right,
                        ) = (top, bottom, left, right);
                    });
                });
                Ok((changed, format!("Margins: {item}")))
            }
            PickerKind::Orientation => {
                let landscape = item == "Landscape";
                let ed = self.body_editor_mut();
                let k = ed.target_sections();
                let changed = ed.edit_section_setups(&k, |s| s.set_landscape(landscape));
                Ok((changed, format!("Orientation: {item}")))
            }
            PickerKind::PaperSize => {
                let paper = Paper::ALL
                    .into_iter()
                    .find(|p| p.label() == item)
                    .ok_or_else(|| format!("Unknown size: {item}"))?;
                let ed = self.body_editor_mut();
                let k = ed.target_sections();
                let changed = ed.edit_section_setups(&k, |s| s.page.set_paper(paper));
                Ok((changed, format!("Size: {}", paper.label())))
            }
            PickerKind::Columns => {
                let gutter_at_top = self.pkg.has_gutter_at_top();
                let ed = self.body_editor_mut();
                let k = ed.target_sections();
                let changed = ed.edit_section_setups(&k, |s| {
                    s.columns = match item {
                        "One" => s.columns.equal(1, 720),
                        "Two" => s.columns.equal(2, 720),
                        "Three" => s.columns.equal(3, 720),
                        "Left" => s.columns.two_unequal(s.text_width(gutter_at_top), true),
                        _ => s.columns.two_unequal(s.text_width(gutter_at_top), false),
                    }
                });
                Ok((changed, format!("Columns: {item}")))
            }
            PickerKind::LineNumbers if item.starts_with("Suppress") => {
                self.body_editor_mut().toggle_suppress_line_numbers();
                Ok((true, "Suppress line numbers toggled".into()))
            }
            PickerKind::LineNumbers => {
                let restart = match item {
                    "Continuous" => Some(LnRestart::Continuous),
                    "Restart Each Page" => Some(LnRestart::NewPage),
                    "Restart Each Section" => Some(LnRestart::NewSection),
                    _ => None,
                };
                let ed = self.body_editor_mut();
                let k = ed.target_sections();
                let changed = ed.edit_section_setups(&k, |s| {
                    let Some(restart) = restart else {
                        s.line_numbers = None;
                        return;
                    };
                    let old = s.line_numbers;
                    s.line_numbers = Some(LineNumbering {
                        count_by: 1,
                        start: old.and_then(|l| l.start),
                        distance: old.and_then(|l| l.distance),
                        restart,
                    });
                });
                Ok((changed, format!("Line numbers: {item}")))
            }
            PickerKind::Hyphenation => {
                let on = item == "Automatic";
                let mut changed = false;
                if self.pkg.has_auto_hyphenation() != on {
                    self.pkg.set_auto_hyphenation(on);
                    if self.pkg.has_auto_hyphenation() != on {
                        return Err(SETTINGS_UNREADABLE.into());
                    }
                    self.modified = true;
                    changed = true;
                }
                let status = format!("Automatic hyphenation: {}", if on { "on" } else { "off" });
                Ok((changed, status))
            }
            PickerKind::Breaks => {
                if let Some(start) = section_start(item) {
                    if self.hf_edit.is_some() {
                        return Err("A section break cannot go in a header or footer".into());
                    }
                    self.editor.insert_section_break(start)?;
                    return Ok((true, format!("Section break ({})", start.val())));
                }
                let kind = match item {
                    "Page" => BreakKind::Page,
                    "Column" => BreakKind::Column,
                    _ => BreakKind::Clear(ClearKind::All),
                };
                // A page, column or wrapping break goes where the caret is
                // typing: `self.editor` is the open header or footer's
                // editor, else the body's.
                self.editor.insert_break(kind);
                Ok((true, format!("{item} break")))
            }
            _ => unreachable!("not a Layout picker: {kind:?}"),
        }
    }
}
