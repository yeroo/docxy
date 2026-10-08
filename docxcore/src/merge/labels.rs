//! Labels (Mailings ▸ Create ▸ Labels…, Start Mail Merge ▸ Labels…, Update
//! Labels): a fixed-layout table with one cell per label on the sheet, and
//! the page set to the sheet's size and margins so the cells line up with
//! the labels when printed.

use super::fields::{MergeFieldKind, field_kind, rule_field};
use crate::model::{Block, Cell, Inline, Paragraph, Row, Run, RunProps, Table};
use crate::sect::{Margins, PageSize, SectionSetup};
use crate::table_props::{PropsXml, TBLPR_ORDER};

/// A label sheet's layout, in twips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelSpec {
    pub name: &'static str,
    pub page_w: i32,
    pub page_h: i32,
    /// Distance from the top of the sheet to the first label.
    pub top: i32,
    /// Distance from the left of the sheet to the first label.
    pub side: i32,
    pub label_w: i32,
    pub label_h: i32,
    /// Left edge to left edge of neighbouring labels.
    pub h_pitch: i32,
    /// Top edge to top edge of neighbouring labels.
    pub v_pitch: i32,
    pub across: usize,
    pub down: usize,
}

const IN: i32 = 1440;
const LETTER: (i32, i32) = (12240, 15840);
const A4: (i32, i32) = (11906, 16838);

/// mm to twips, rounded.
const fn mm(tenths: i32) -> i32 {
    (tenths * 1440 + 127) / 254
}

/// The label products the Label Options list offers, with the dimensions
/// Avery publishes for each sheet (Avery US: Letter; Avery A4/A5: A4).
pub const LABEL_PRESETS: [LabelSpec; 5] = [
    // Avery 5160 Address: 1" × 2 5/8", 3 × 10; top 1/2", side 3/16",
    // horizontal pitch 2 3/4", vertical pitch 1".
    LabelSpec {
        name: "Avery US Letter 5160 Address Labels",
        page_w: LETTER.0,
        page_h: LETTER.1,
        top: IN / 2,
        side: IN * 3 / 16,
        label_w: IN * 21 / 8,
        label_h: IN,
        h_pitch: IN * 11 / 4,
        v_pitch: IN,
        across: 3,
        down: 10,
    },
    // Avery 5163 Shipping: 2" × 4", 2 × 5; top 1/2", side 5/32",
    // horizontal pitch 4 3/16", vertical pitch 2".
    LabelSpec {
        name: "Avery US Letter 5163 Shipping Labels",
        page_w: LETTER.0,
        page_h: LETTER.1,
        top: IN / 2,
        side: IN * 5 / 32,
        label_w: IN * 4,
        label_h: IN * 2,
        h_pitch: IN * 67 / 16,
        v_pitch: IN * 2,
        across: 2,
        down: 5,
    },
    // Avery 5167 Return Address: 1/2" × 1 3/4", 4 × 20; top 1/2", side
    // 9/32", horizontal pitch 2 1/16", vertical pitch 1/2".
    LabelSpec {
        name: "Avery US Letter 5167 Return Address Labels",
        page_w: LETTER.0,
        page_h: LETTER.1,
        top: IN / 2,
        side: IN * 9 / 32,
        label_w: IN * 7 / 4,
        label_h: IN / 2,
        h_pitch: IN * 33 / 16,
        v_pitch: IN / 2,
        across: 4,
        down: 20,
    },
    // Avery L7160: 63.5 × 38.1 mm, 3 × 7; top 15.15 mm, side 7.25 mm,
    // horizontal pitch 66.04 mm, vertical pitch 38.1 mm.
    LabelSpec {
        name: "Avery A4/A5 L7160 Address Labels",
        page_w: A4.0,
        page_h: A4.1,
        top: mm(1515) / 10,
        side: mm(725) / 10,
        label_w: mm(635),
        label_h: mm(381),
        h_pitch: mm(6604) / 10,
        v_pitch: mm(381),
        across: 3,
        down: 7,
    },
    // Avery L7163: 99.1 × 38.1 mm, 2 × 7; top 15.15 mm, side 4.65 mm,
    // horizontal pitch 101.6 mm, vertical pitch 38.1 mm.
    LabelSpec {
        name: "Avery A4/A5 L7163 Address Labels",
        page_w: A4.0,
        page_h: A4.1,
        top: mm(1515) / 10,
        side: mm(465) / 10,
        label_w: mm(991),
        label_h: mm(381),
        h_pitch: mm(1016),
        v_pitch: mm(381),
        across: 2,
        down: 7,
    },
];

impl LabelSpec {
    /// A custom label: the Letter sheet with the given grid, pitch = size.
    pub fn custom(label_w: i32, label_h: i32, across: usize, down: usize) -> LabelSpec {
        LabelSpec {
            name: "Custom",
            page_w: LETTER.0,
            page_h: LETTER.1,
            top: IN / 2,
            side: IN / 4,
            label_w,
            label_h,
            h_pitch: label_w,
            v_pitch: label_h,
            across: across.max(1),
            down: down.max(1),
        }
    }

    /// The gap between neighbouring labels, filled by a spacer column.
    pub fn spacer_w(&self) -> i32 {
        (self.h_pitch - self.label_w).max(0)
    }

    /// The table's column widths: label, spacer, label, … (no spacer when
    /// the labels touch).
    pub fn grid(&self) -> Vec<u32> {
        let mut grid = Vec::new();
        for i in 0..self.across {
            if i > 0 && self.spacer_w() > 0 {
                grid.push(self.spacer_w() as u32);
            }
            grid.push(self.label_w.max(1) as u32);
        }
        grid
    }

    /// Set a section to the sheet: its page size (portrait) and margins.
    pub fn apply_page(&self, sect: &str) -> String {
        let mut setup = SectionSetup::parse(sect);
        setup.page = PageSize {
            w: self.page_w,
            h: self.page_h,
            landscape: false,
            code: setup.page.code,
        };
        let width: i32 = self.grid().iter().map(|&w| w as i32).sum();
        setup.margins = Margins {
            top: self.top,
            left: self.side,
            right: (self.page_w - self.side - width).max(0),
            bottom: 0,
            header: 0,
            footer: 0,
            gutter: 0,
        };
        setup.columns = Default::default();
        setup.apply(sect)
    }
}

/// What the labels hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelFill {
    /// The same text on every label ("Full page of the same label").
    Same(String),
    /// For a merge: the first label empty, every other one starting with a
    /// `NEXT` field, ready for an Address Block and Update Labels.
    Merge,
}

fn paragraph(content: Vec<Inline>) -> Block {
    Block::Paragraph(Paragraph {
        content,
        ..Default::default()
    })
}

fn cell(width: i32, blocks: Vec<Block>) -> Cell {
    Cell {
        blocks,
        raw_tcpr: Some(format!(
            "<w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/></w:tcPr>"
        )),
        ..Cell::default()
    }
}

/// The label table: fixed layout, rows of exactly the vertical pitch, label
/// columns with spacer columns between them when the labels do not touch,
/// no borders (the gridlines show on screen only).
pub fn label_table(spec: &LabelSpec, fill: &LabelFill) -> Table {
    let grid = spec.grid();
    let width: u32 = grid.iter().sum();
    let mut tblpr = PropsXml::new("w:tblPr", TBLPR_ORDER);
    tblpr.set(&format!("<w:tblW w:w=\"{width}\" w:type=\"dxa\"/>"));
    tblpr.set("<w:tblLayout w:type=\"fixed\"/>");
    tblpr.set(
        "<w:tblCellMar><w:left w:w=\"15\" w:type=\"dxa\"/>\
         <w:right w:w=\"15\" w:type=\"dxa\"/></w:tblCellMar>",
    );
    tblpr.set(
        "<w:tblLook w:val=\"0000\" w:firstRow=\"0\" w:lastRow=\"0\" w:firstColumn=\"0\" \
         w:lastColumn=\"0\" w:noHBand=\"0\" w:noVBand=\"0\"/>",
    );
    let props = RunProps::default();
    let mut first = true;
    let mut label = || -> Vec<Block> {
        let blocks = match fill {
            LabelFill::Same(text) => {
                let lines: Vec<Block> = text
                    .split(['\n', '\r'])
                    .filter(|l| !l.is_empty())
                    .map(|l| {
                        paragraph(vec![Inline::Run(Run {
                            text: l.to_string(),
                            props: props.clone(),
                        })])
                    })
                    .collect();
                if lines.is_empty() {
                    vec![paragraph(Vec::new())]
                } else {
                    lines
                }
            }
            LabelFill::Merge if first => vec![paragraph(Vec::new())],
            LabelFill::Merge => vec![paragraph(vec![
                rule_field(&MergeFieldKind::Next, &props).expect("NEXT is a rule"),
            ])],
        };
        first = false;
        blocks
    };
    let spacer = spec.spacer_w();
    let rows = (0..spec.down)
        .map(|_| {
            let mut cells = Vec::new();
            for c in 0..spec.across {
                if c > 0 && spacer > 0 {
                    cells.push(cell(spacer, vec![paragraph(Vec::new())]));
                }
                cells.push(cell(spec.label_w, label()));
            }
            Row {
                cells,
                raw_props: vec![format!(
                    "<w:trPr><w:cantSplit/><w:trHeight w:val=\"{}\" w:hRule=\"exact\"/></w:trPr>",
                    spec.v_pitch.max(spec.label_h)
                )],
                ..Row::default()
            }
        })
        .collect();
    Table {
        grid,
        rows,
        raw_tblpr: Some(tblpr.to_xml()),
        ..Table::default()
    }
}

/// Update Labels: copy the first label's content into every other label
/// cell, each starting with one `NEXT` field (an existing leading `NEXT` is
/// not doubled); spacer cells stay as they are. Whether anything changed.
pub fn update_labels(table: &mut Table) -> bool {
    let Some(source) = table
        .rows
        .first()
        .and_then(|r| r.cells.first())
        .map(|c| c.blocks.clone())
    else {
        return false;
    };
    let mut source = source;
    strip_leading_next(&mut source);
    let next = rule_field(&MergeFieldKind::Next, &RunProps::default()).expect("NEXT is a rule");
    let mut changed = false;
    let mut first = true;
    for row in &mut table.rows {
        let mut col = 0;
        for cell in &mut row.cells {
            let span = cell.grid_span.max(1) as usize;
            let label = is_label_column_at(&table.grid, col);
            col += span;
            if !label {
                continue;
            }
            if first {
                first = false;
                continue;
            }
            let mut blocks = source.clone();
            match blocks.first_mut() {
                Some(Block::Paragraph(p)) => p.content.insert(0, next.clone()),
                _ => blocks.insert(0, paragraph(vec![next.clone()])),
            }
            if cell.blocks != blocks {
                cell.blocks = blocks;
                changed = true;
            }
        }
    }
    changed
}

/// Replace the document with a sheet of labels: the label table (and the
/// paragraph Word keeps after a table), with the page set to the sheet, as
/// one undo step.
pub fn insert_labels(editor: &mut crate::editor::Editor, spec: &LabelSpec, fill: &LabelFill) {
    let blocks = vec![Block::Table(label_table(spec, fill)), paragraph(Vec::new())];
    editor.replace_body(blocks, |sect| spec.apply_page(sect));
}

/// Update Labels on the table holding the caret, as one undo step.
pub fn update_labels_at_caret(editor: &mut crate::editor::Editor) -> Result<bool, String> {
    editor
        .edit_table_at_caret(update_labels)
        .map_err(|_| "Put the cursor in the label table first".to_string())
}

/// Whether grid column `c` of a label table is a label (not a spacer): the
/// label columns are as wide as the first one.
fn is_label_column_at(grid: &[u32], c: usize) -> bool {
    grid.get(c) == grid.first()
}

/// Drop `NEXT` fields at the start of the first paragraph, smart-tag
/// boundaries (which show nothing) aside.
fn strip_leading_next(blocks: &mut [Block]) {
    if let Some(Block::Paragraph(p)) = blocks.first_mut() {
        let mut i = 0;
        loop {
            match p.content.get(i) {
                Some(Inline::Field { raw, .. })
                    if field_kind(raw) == Some(MergeFieldKind::Next) =>
                {
                    p.content.remove(i);
                }
                Some(Inline::Raw(raw))
                    if crate::hf::is_smart_tag_open(raw) || crate::hf::is_smart_tag_close(raw) =>
                {
                    i += 1;
                }
                _ => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trh(row: &Row) -> &str {
        &row.raw_props[0]
    }

    fn cell_text(c: &Cell) -> String {
        c.blocks
            .iter()
            .map(|b| b.plain_text())
            .collect::<Vec<_>>()
            .join("|")
    }

    fn is_next(i: &Inline) -> bool {
        matches!(i, Inline::Field { raw, .. } if field_kind(raw) == Some(MergeFieldKind::Next))
    }

    #[test]
    fn avery_5160_grid_rows_and_page() {
        let spec = LABEL_PRESETS[0];
        let t = label_table(&spec, &LabelFill::Same("Jane Doe\n1 Main St".into()));
        // 2 5/8" labels, 1/8" spacers.
        assert_eq!(t.grid, [3780, 180, 3780, 180, 3780]);
        assert_eq!(t.rows.len(), 10);
        assert_eq!(t.rows[0].cells.len(), 5);
        assert!(trh(&t.rows[0]).contains("<w:trHeight w:val=\"1440\" w:hRule=\"exact\"/>"));
        let tblpr = t.raw_tblpr.as_deref().unwrap();
        assert!(tblpr.contains("<w:tblLayout w:type=\"fixed\"/>"), "{tblpr}");
        assert!(
            tblpr.contains("<w:tblW w:w=\"11700\" w:type=\"dxa\"/>"),
            "{tblpr}"
        );
        assert_eq!(cell_text(&t.rows[3].cells[4]), "Jane Doe|1 Main St");
        assert_eq!(cell_text(&t.rows[3].cells[1]), "", "spacer");
        assert!(
            t.rows[0].cells[1]
                .raw_tcpr
                .as_deref()
                .unwrap()
                .contains("w:w=\"180\"")
        );
        let sect = spec.apply_page("<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr>");
        let s = SectionSetup::parse(&sect);
        assert_eq!((s.page.w, s.page.h), (12240, 15840));
        assert_eq!((s.margins.top, s.margins.left), (720, 270));
        assert_eq!(s.margins.right, 12240 - 270 - 11700);
        assert_eq!(s.margins.bottom, 0);
    }

    #[test]
    fn insert_labels_replaces_the_body_and_update_labels_is_one_undo_step() {
        use crate::editor::Editor;
        use crate::model::{Document, SectionProperties};
        let mut doc = Document {
            body: vec![paragraph(vec![Inline::Run(Run {
                text: "old".into(),
                props: RunProps::default(),
            })])],
        };
        doc.set_trailing_section_properties(SectionProperties {
            raw: "<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId3\"/>\
                  <w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr>"
                .into(),
            property_change: None,
        });
        let mut ed = Editor::new(doc);
        insert_labels(&mut ed, &LABEL_PRESETS[0], &LabelFill::Merge);
        assert!(matches!(ed.doc.body[0], Block::Table(_)));
        assert!(!ed.doc.plain_text().contains("old"));
        let sections = ed.sections();
        assert_eq!(sections.len(), 1);
        let s = SectionSetup::parse(&sections[0]);
        assert_eq!((s.page.w, s.margins.top, s.margins.left), (12240, 720, 270));
        assert!(sections[0].contains("headerReference"), "{}", sections[0]);
        assert_eq!(ed.caret.path, [0, 0, 0, 0]);
        // Type into the first label, then Update Labels.
        ed.insert_str("Jane");
        assert_eq!(update_labels_at_caret(&mut ed), Ok(true));
        let Block::Table(t) = &ed.doc.body[0] else {
            panic!()
        };
        assert_eq!(
            cell_text(&t.rows[9].cells[4]),
            "\u{AB}Next Record\u{BB}Jane"
        );
        assert!(ed.undo());
        let Block::Table(t) = &ed.doc.body[0] else {
            panic!()
        };
        assert_eq!(cell_text(&t.rows[9].cells[4]), "\u{AB}Next Record\u{BB}");
        assert!(ed.undo());
        assert!(ed.undo());
        assert!(ed.doc.plain_text().contains("old"));
        // Outside a table there is nothing to update.
        assert_eq!(
            update_labels_at_caret(&mut ed),
            Err("Put the cursor in the label table first".into())
        );
        assert_eq!(
            ed.edit_table_at_caret(|_| true),
            Err("the caret is not in a table".into())
        );
    }

    #[test]
    fn presets_fit_their_sheets() {
        for spec in LABEL_PRESETS {
            let width: i32 = spec.grid().iter().map(|&w| w as i32).sum();
            assert!(spec.side + width <= spec.page_w, "{}", spec.name);
            assert!(
                spec.top + spec.v_pitch * spec.down as i32 <= spec.page_h,
                "{}",
                spec.name
            );
        }
        // L7163's labels nearly touch: 99.1 mm wide at a 101.6 mm pitch.
        let l7163 = LABEL_PRESETS[4];
        assert_eq!(l7163.grid(), [5618, 142, 5618]);
        // Touching custom labels have no spacer columns.
        assert_eq!(
            LabelSpec::custom(2880, 1440, 3, 4).grid(),
            [2880, 2880, 2880]
        );
    }

    #[test]
    fn merge_fill_puts_next_in_every_label_but_the_first() {
        let t = label_table(&LABEL_PRESETS[1], &LabelFill::Merge);
        let first = &t.rows[0].cells[0];
        assert_eq!(cell_text(first), "");
        let Block::Paragraph(p) = &t.rows[0].cells[2].blocks[0] else {
            panic!()
        };
        assert!(is_next(&p.content[0]));
        let Block::Paragraph(p) = &t.rows[0].cells[1].blocks[0] else {
            panic!()
        };
        assert!(p.content.is_empty(), "spacer cells hold no NEXT");
    }

    #[test]
    fn update_labels_copies_the_first_label_behind_next() {
        let mut t = label_table(&LABEL_PRESETS[1], &LabelFill::Merge);
        let ab = crate::merge::address_block_field(true, &RunProps::default());
        let Block::Paragraph(p) = &mut t.rows[0].cells[0].blocks[0] else {
            panic!()
        };
        p.content.push(ab.clone());
        assert!(update_labels(&mut t));
        for (r, row) in t.rows.iter().enumerate() {
            for (c, cell) in row.cells.iter().enumerate() {
                let Block::Paragraph(p) = &cell.blocks[0] else {
                    panic!()
                };
                match (r, c) {
                    (0, 0) => assert_eq!(p.content, std::slice::from_ref(&ab)),
                    (_, 1) => assert!(p.content.is_empty(), "spacer"),
                    _ => {
                        assert_eq!(p.content.len(), 2, "({r},{c})");
                        assert!(is_next(&p.content[0]));
                        assert_eq!(p.content[1], ab);
                    }
                }
            }
        }
        // Again: nothing doubles.
        assert!(!update_labels(&mut t));
        // Not even when the first label itself starts with NEXT.
        let Block::Paragraph(p) = &mut t.rows[0].cells[0].blocks[0] else {
            panic!()
        };
        p.content.insert(
            0,
            rule_field(&MergeFieldKind::Next, &RunProps::default()).unwrap(),
        );
        update_labels(&mut t);
        let Block::Paragraph(p) = &t.rows[1].cells[0].blocks[0] else {
            panic!()
        };
        assert_eq!(p.content.iter().filter(|i| is_next(i)).count(), 1);
        // Nor when that NEXT is inside a smart tag the label starts with
        // (#1069).
        let Block::Paragraph(p) = &mut t.rows[0].cells[0].blocks[0] else {
            panic!()
        };
        p.content
            .insert(0, Inline::Raw("<w:smartTag w:element=\"x\">".into()));
        p.content
            .push(Inline::Raw(crate::hf::SMART_TAG_CLOSE.into()));
        update_labels(&mut t);
        let Block::Paragraph(p) = &t.rows[1].cells[0].blocks[0] else {
            panic!()
        };
        assert_eq!(p.content.iter().filter(|i| is_next(i)).count(), 1);
    }
}
