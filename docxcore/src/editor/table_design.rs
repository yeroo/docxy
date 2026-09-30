//! The Table Design tab's commands (#648): table style, style options
//! (`w:tblLook`), cell shading and borders.
//!
//! A table style is written as `w:tblStyle` only; its definition is added to
//! `styles.xml` when the package is saved (see
//! [`crate::table_styles::with_table_styles`]), so undo stays a document
//! edit.

use super::Editor;
use crate::model::{Table, VMerge};
use crate::table::{GridMap, cell_props, edit_cell_props, edit_table_props, table_props};
use crate::table_props::{BorderLine, Edge, TblLook, shd_fill, with_border};

use super::tables::CellRange;

/// The Borders menu's commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderCmd {
    Bottom,
    Top,
    Left,
    Right,
    NoBorder,
    All,
    Outside,
    Inside,
    InsideH,
    InsideV,
    DiagDown,
    DiagUp,
}

impl BorderCmd {
    pub const ALL: [BorderCmd; 12] = [
        BorderCmd::Bottom,
        BorderCmd::Top,
        BorderCmd::Left,
        BorderCmd::Right,
        BorderCmd::NoBorder,
        BorderCmd::All,
        BorderCmd::Outside,
        BorderCmd::Inside,
        BorderCmd::InsideH,
        BorderCmd::InsideV,
        BorderCmd::DiagDown,
        BorderCmd::DiagUp,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BorderCmd::Bottom => "Bottom Border",
            BorderCmd::Top => "Top Border",
            BorderCmd::Left => "Left Border",
            BorderCmd::Right => "Right Border",
            BorderCmd::NoBorder => "No Border",
            BorderCmd::All => "All Borders",
            BorderCmd::Outside => "Outside Borders",
            BorderCmd::Inside => "Inside Borders",
            BorderCmd::InsideH => "Inside Horizontal Border",
            BorderCmd::InsideV => "Inside Vertical Border",
            BorderCmd::DiagDown => "Diagonal Down Border",
            BorderCmd::DiagUp => "Diagonal Up Border",
        }
    }

    /// The range edges the command sets.
    fn edges(self) -> &'static [RangeEdge] {
        use RangeEdge::*;
        match self {
            BorderCmd::Bottom => &[Bottom],
            BorderCmd::Top => &[Top],
            BorderCmd::Left => &[Left],
            BorderCmd::Right => &[Right],
            BorderCmd::NoBorder | BorderCmd::All => &[Top, Bottom, Left, Right, InsideH, InsideV],
            BorderCmd::Outside => &[Top, Bottom, Left, Right],
            BorderCmd::Inside => &[InsideH, InsideV],
            BorderCmd::InsideH => &[InsideH],
            BorderCmd::InsideV => &[InsideV],
            BorderCmd::DiagDown => &[DiagDown],
            BorderCmd::DiagUp => &[DiagUp],
        }
    }
}

/// An edge of a selected cell range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeEdge {
    Top,
    Bottom,
    Left,
    Right,
    InsideH,
    InsideV,
    DiagDown,
    DiagUp,
}

/// One cell side a border command touches: `(row, cell, side)`.
type Side = (usize, usize, Edge);

/// The cell sides that make up `edge` of range `r`.
fn sides(t: &Table, map: &GridMap, r: &CellRange, edge: RangeEdge) -> Vec<Side> {
    let mut out = Vec::new();
    for (ri, ci) in r.cells(map) {
        let (s, n) = map.rows[ri].cells[ci];
        let cell = &t.rows[ri].cells[ci];
        // The side between a merged cell's rows is inside that cell.
        let continued_below = map
            .rows
            .get(ri + 1)
            .and_then(|m| m.cell_starting(s))
            .is_some_and(|b| t.rows[ri + 1].cells[b].v_merge == VMerge::Continue);
        let continues = cell.v_merge == VMerge::Continue;
        let (top, bottom, left, right) = (
            ri == r.top,
            ri == r.bottom,
            s == r.left,
            s + n - 1 == r.right,
        );
        let side = match edge {
            RangeEdge::Top if top => Some(Edge::Top),
            RangeEdge::Bottom if bottom => Some(Edge::Bottom),
            RangeEdge::Left if left => Some(Edge::Left),
            RangeEdge::Right if right => Some(Edge::Right),
            RangeEdge::DiagDown if !continues => Some(Edge::DiagDown),
            RangeEdge::DiagUp if !continues => Some(Edge::DiagUp),
            _ => None,
        };
        if let Some(e) = side {
            out.push((ri, ci, e));
        }
        match edge {
            RangeEdge::InsideH => {
                if !top && !continues {
                    out.push((ri, ci, Edge::Top));
                }
                if !bottom && !continued_below {
                    out.push((ri, ci, Edge::Bottom));
                }
            }
            RangeEdge::InsideV => {
                if !left {
                    out.push((ri, ci, Edge::Left));
                }
                if !right {
                    out.push((ri, ci, Edge::Right));
                }
            }
            _ => {}
        }
    }
    out
}

/// The cell sides facing an outer edge of `r` from outside it.
fn facing(t: &Table, map: &GridMap, r: &CellRange, edge: RangeEdge) -> Vec<Side> {
    let mut out = Vec::new();
    let mut push = |ri: usize, col: usize, e: Edge| {
        if let Some(ci) = map.rows.get(ri).and_then(|m| m.cell_at(col)) {
            if !out.contains(&(ri, ci, e)) {
                out.push((ri, ci, e));
            }
        }
    };
    match edge {
        RangeEdge::Top if r.top > 0 => {
            for col in r.left..=r.right {
                push(r.top - 1, col, Edge::Bottom);
            }
        }
        RangeEdge::Bottom if r.bottom + 1 < t.rows.len() => {
            for col in r.left..=r.right {
                push(r.bottom + 1, col, Edge::Top);
            }
        }
        RangeEdge::Left if r.left > 0 => {
            for ri in r.top..=r.bottom {
                push(ri, r.left - 1, Edge::Right);
            }
        }
        RangeEdge::Right => {
            for ri in r.top..=r.bottom {
                push(ri, r.right + 1, Edge::Left);
            }
        }
        _ => {}
    }
    out
}

/// Every cell's resolved sides: its own `tcBorders`, the table's
/// `tblBorders`, then the table style when it is one of the built-ins (the
/// editor has no `styles.xml`; a document's own style is not seen here).
fn looks(t: &Table) -> Vec<Vec<crate::table_styles::CellLook>> {
    let style = table_props(t)
        .attr("w:tblStyle", "w:val")
        .and_then(|id| crate::table_styles::lookup_style(None, &id));
    crate::table_styles::resolve(t, style.as_ref())
}

fn set_side(t: &mut Table, (ri, ci, e): Side, line: Option<&BorderLine>) {
    edit_cell_props(&mut t.rows[ri].cells[ci], |p| {
        match with_border(p.get("w:tcBorders"), "w:tcBorders", e, line) {
            Some(g) => p.set(&g),
            None => {
                p.remove("w:tcBorders");
            }
        }
    });
}

impl Editor {
    /// The caret's table's style id.
    pub fn table_style(&self) -> Option<String> {
        let r = self.table_selection()?;
        table_props(self.table(&r.table)?).attr("w:tblStyle", "w:val")
    }

    /// Apply table style `id` (`w:tblStyle`), with Word's default style
    /// options when the table has none. The style's definition is added to
    /// `styles.xml` when the document is saved.
    pub fn set_table_style(&mut self, id: &str) -> Result<(), String> {
        if id.is_empty() || id.contains(['"', '<', '>', '&']) {
            return Err("not a style id".into());
        }
        let r = self
            .table_selection()
            .ok_or("the caret is not in a table")?;
        self.edit_table_props_at(&r.table, |t| {
            edit_table_props(t, |p| {
                p.set(&format!("<w:tblStyle w:val=\"{id}\"/>"));
                // Word's gallery click clears table-level borders, so the
                // style's own borders show.
                p.remove("w:tblBorders");
                if p.get("w:tblLook").is_none() {
                    p.set(&TblLook::default().to_xml());
                }
            })
        })
    }

    /// The caret's table's style options.
    pub fn table_look(&self) -> Option<TblLook> {
        let r = self.table_selection()?;
        Some(
            table_props(self.table(&r.table)?)
                .get("w:tblLook")
                .map(TblLook::parse)
                .unwrap_or_default(),
        )
    }

    /// Table Style Options: write `w:tblLook` (hex mask and attributes).
    pub fn set_table_look(&mut self, look: TblLook) -> Result<(), String> {
        let r = self
            .table_selection()
            .ok_or("the caret is not in a table")?;
        self.edit_table_props_at(&r.table, |t| edit_table_props(t, |p| p.set(&look.to_xml())))
    }

    /// The caret cell's direct fill (`RRGGBB`).
    pub fn cell_shading(&self) -> Option<String> {
        let pos = self.table_at_caret()?;
        let cell = self
            .table(&pos.table)?
            .rows
            .get(pos.row)?
            .cells
            .get(pos.cell)?;
        shd_fill(cell_props(cell).get("w:shd")).0
    }

    /// Shading: fill every selected cell with `fill` (`RRGGBB`), or with
    /// nothing (`None`, Word's No Color, which also hides a style's fill).
    pub fn set_cell_shading(&mut self, fill: Option<&str>) -> Result<(), String> {
        if let Some(f) = fill {
            if f.len() != 6 || !f.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err("a fill is six hex digits".into());
            }
        }
        let r = self
            .table_selection()
            .ok_or("the caret is not in a table")?;
        let fill = fill.map_or_else(|| "auto".to_string(), str::to_ascii_uppercase);
        self.edit_table_props_at(&r.table.clone(), |t| {
            let map = GridMap::of(t);
            for (ri, ci) in r.cells(&map) {
                edit_cell_props(&mut t.rows[ri].cells[ci], |p| {
                    p.set(&format!(
                        "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{fill}\"/>"
                    ))
                });
            }
        })
    }

    /// Whether every side a border command sets already shows a line (the
    /// button is "on"). Only a cell's own borders and the table's are
    /// considered, not the table style's.
    pub fn border_state(&self, cmd: BorderCmd) -> bool {
        if cmd == BorderCmd::NoBorder {
            return false;
        }
        let Some(r) = self.table_selection() else {
            return false;
        };
        let Some(t) = self.table(&r.table) else {
            return false;
        };
        let map = GridMap::of(t);
        let all: Vec<Side> = cmd
            .edges()
            .iter()
            .flat_map(|&e| sides(t, &map, &r, e))
            .collect();
        let looks = looks(t);
        !all.is_empty()
            && all
                .iter()
                .all(|&(ri, ci, e)| looks[ri][ci].visible(e).is_some())
    }

    /// The Borders menu: toggle the command's sides on the selected cells as
    /// Word does. A side turned off is `nil` (so neither the table's borders
    /// nor its style show through), and so is the adjacent cell's side
    /// facing a removed outer edge. No Border turns off every side and
    /// removes the diagonals.
    pub fn apply_borders(&mut self, cmd: BorderCmd) -> Result<(), String> {
        let r = self
            .table_selection()
            .ok_or("the caret is not in a table")?;
        let on = cmd != BorderCmd::NoBorder && !self.border_state(cmd);
        let path = r.table.clone();
        self.edit_table_props_at(&path, |t| {
            let map = GridMap::of(t);
            let line = on.then(BorderLine::single);
            for &edge in cmd.edges() {
                let diagonal = matches!(edge, RangeEdge::DiagDown | RangeEdge::DiagUp);
                let value = match (&line, diagonal) {
                    (Some(l), _) => Some(l.clone()),
                    (None, true) => None,
                    (None, false) => Some(BorderLine::nil()),
                };
                for side in sides(t, &map, &r, edge) {
                    set_side(t, side, value.as_ref());
                }
                if !on && !diagonal {
                    for side in facing(t, &map, &r, edge) {
                        set_side(t, side, Some(&BorderLine::nil()));
                    }
                }
            }
            if cmd == BorderCmd::NoBorder {
                for e in [RangeEdge::DiagDown, RangeEdge::DiagUp] {
                    for side in sides(t, &map, &r, e) {
                        set_side(t, side, None);
                    }
                }
            }
        })
    }

    /// Edit the table at `path` in place (the cells do not move, so the caret
    /// and selection stay) through [`Editor::edit_table`]: one undo step, none
    /// when nothing changed.
    fn edit_table_props_at(
        &mut self,
        path: &[usize],
        edit: impl FnOnce(&mut Table),
    ) -> Result<(), String> {
        self.edit_table(path, |t| {
            edit(t);
            Ok(super::table_layout::After::Stay)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::Caret;
    use super::super::tables::tests::grid_doc;
    use super::*;
    use crate::model::Block;
    use crate::package::{load_package, new_package, save_package};

    fn t(ed: &Editor) -> &Table {
        ed.table(&[0]).unwrap()
    }

    fn tc(ed: &Editor, r: usize, c: usize) -> String {
        t(ed).rows[r].cells[c].raw_tcpr.clone().unwrap_or_default()
    }

    fn select(ed: &mut Editor, a: (usize, usize), b: (usize, usize)) {
        ed.anchor = Some(Caret::at(vec![0, a.0, a.1, 0], 0));
        ed.caret = Caret::at(vec![0, b.0, b.1, 0], 0);
    }

    #[test]
    fn style_and_options_write_tbl_style_and_tbl_look() {
        let mut ed = Editor::new(grid_doc(2, 2));
        ed.set_table_style("GridTable4-Accent1").unwrap();
        assert_eq!(ed.table_style().as_deref(), Some("GridTable4-Accent1"));
        let p = t(&ed).raw_tblpr.clone().unwrap();
        assert!(p.starts_with("<w:tblPr><w:tblStyle w:val=\"GridTable4-Accent1\"/>"));
        assert!(p.contains("w:val=\"04A0\""));
        let look = TblLook {
            last_row: true,
            banded_rows: false,
            ..TblLook::default()
        };
        ed.set_table_look(look).unwrap();
        assert_eq!(ed.table_look(), Some(look));
        let p = t(&ed).raw_tblpr.clone().unwrap();
        assert!(p.contains("w:val=\"06E0\"") && p.contains("w:lastRow=\"1\" "));
        assert!(p.contains("w:noHBand=\"1\""));
        assert!(ed.undo());
        assert_eq!(ed.table_look(), Some(TblLook::default()));
        assert!(ed.set_table_style("bad\"id").is_err());
    }

    #[test]
    fn a_new_table_is_table_grid_and_a_gallery_style_clears_direct_borders() {
        let mut ed = Editor::new(grid_doc(2, 2));
        assert_eq!(ed.table_style().as_deref(), Some("TableGrid"));
        assert!(
            ed.border_state(BorderCmd::All),
            "Table Grid draws every border"
        );
        // Direct borders from an older document go when a style is picked.
        crate::table::edit_table_props(
            super::super::tables::table_at_mut(&mut ed.doc.body, &[0]).unwrap(),
            |p| {
                p.set("<w:tblBorders><w:top w:val=\"single\" w:sz=\"4\" w:color=\"auto\"/></w:tblBorders>")
            },
        );
        ed.set_table_style("PlainTable4").unwrap();
        let p = t(&ed).raw_tblpr.clone().unwrap();
        assert!(!p.contains("w:tblBorders"), "{p}");
        assert!(
            !ed.border_state(BorderCmd::Top),
            "Plain Table 4 has no borders"
        );
        // A fresh package defines Table Grid when saved.
        let mut ed = Editor::new(crate::model::Document {
            body: vec![Block::Paragraph(Default::default())],
        });
        ed.insert_table(2, 2, crate::table::AutoFit::Default)
            .unwrap();
        let pkg = load_package(&save_package(&new_package(ed.doc.clone()))).unwrap();
        let styles = String::from_utf8_lossy(pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(styles.contains("w:styleId=\"TableGrid\""));
    }

    #[test]
    fn saving_adds_the_style_definition_once() {
        let mut ed = Editor::new(grid_doc(2, 2));
        ed.set_table_style("ListTable3-Accent1").unwrap();
        let bytes = save_package(&new_package(ed.doc.clone()));
        let pkg = load_package(&bytes).unwrap();
        let styles = String::from_utf8_lossy(pkg.part("word/styles.xml").unwrap()).into_owned();
        assert_eq!(
            styles.matches("w:styleId=\"ListTable3-Accent1\"").count(),
            1
        );
        assert!(styles.contains("w:styleId=\"TableNormal\""));
        let again = save_package(&pkg);
        let pkg = load_package(&again).unwrap();
        let styles2 = String::from_utf8_lossy(pkg.part("word/styles.xml").unwrap()).into_owned();
        assert_eq!(styles, styles2, "idempotent");
    }

    #[test]
    fn shading_and_no_color() {
        let mut ed = Editor::new(grid_doc(2, 2));
        select(&mut ed, (0, 0), (0, 1));
        ed.set_cell_shading(Some("ff0000")).unwrap();
        for c in 0..2 {
            assert!(
                tc(&ed, 0, c)
                    .contains("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"FF0000\"/>")
            );
        }
        assert!(tc(&ed, 1, 0).is_empty());
        ed.anchor = None;
        ed.caret = Caret::at(vec![0, 0, 0, 0], 0);
        assert_eq!(ed.cell_shading().as_deref(), Some("FF0000"));
        ed.set_cell_shading(None).unwrap();
        assert!(tc(&ed, 0, 0).contains("w:fill=\"auto\""));
        assert_eq!(ed.cell_shading(), None);
        assert!(ed.set_cell_shading(Some("red")).is_err());
    }

    #[test]
    fn border_buttons_toggle_over_the_tables_own_borders() {
        // A new table has single borders everywhere: All is on, so it nils.
        let mut ed = Editor::new(grid_doc(2, 2));
        select(&mut ed, (0, 0), (1, 1));
        assert!(ed.border_state(BorderCmd::All));
        ed.apply_borders(BorderCmd::All).unwrap();
        assert!(!ed.border_state(BorderCmd::All));
        assert!(!ed.border_state(BorderCmd::Top));
        assert!(tc(&ed, 0, 0).contains("<w:top w:val=\"nil\"/>"));
        // Again: back on, explicitly.
        ed.apply_borders(BorderCmd::All).unwrap();
        assert!(ed.border_state(BorderCmd::All));
        assert!(tc(&ed, 1, 1).contains("<w:bottom w:val=\"single\""));
    }

    #[test]
    fn outside_and_inside_edges_of_a_range() {
        let mut ed = Editor::new(grid_doc(3, 3));
        // Clear everything first so each command's effect stands alone.
        ed.select_table().unwrap();
        ed.apply_borders(BorderCmd::NoBorder).unwrap();
        select(&mut ed, (0, 0), (1, 1));
        ed.apply_borders(BorderCmd::Outside).unwrap();
        let has = |ed: &Editor, r, c, tag: &str| {
            tc(ed, r, c).contains(&format!("<w:{tag} w:val=\"single\""))
        };
        assert!(has(&ed, 0, 0, "top") && has(&ed, 0, 0, "left"));
        assert!(!has(&ed, 0, 0, "bottom") && !has(&ed, 0, 0, "right"));
        assert!(has(&ed, 1, 1, "bottom") && has(&ed, 1, 1, "right"));
        assert!(!has(&ed, 2, 2, "top"));
        ed.apply_borders(BorderCmd::InsideH).unwrap();
        assert!(has(&ed, 0, 0, "bottom") && has(&ed, 1, 0, "top"));
        assert!(!has(&ed, 0, 0, "right"));
        ed.apply_borders(BorderCmd::InsideV).unwrap();
        assert!(has(&ed, 0, 0, "right") && has(&ed, 0, 1, "left"));
        assert!(ed.border_state(BorderCmd::All));
    }

    #[test]
    fn removing_an_outer_edge_nils_the_facing_side_too() {
        let mut ed = Editor::new(grid_doc(2, 2));
        ed.caret = Caret::at(vec![0, 1, 0, 0], 0);
        assert!(ed.border_state(BorderCmd::Top));
        ed.apply_borders(BorderCmd::Top).unwrap();
        assert!(tc(&ed, 1, 0).contains("<w:top w:val=\"nil\"/>"));
        assert!(tc(&ed, 0, 0).contains("<w:bottom w:val=\"nil\"/>"));
        assert!(tc(&ed, 0, 1).is_empty(), "other columns are untouched");
    }

    #[test]
    fn diagonals_toggle_and_no_border_removes_them() {
        let mut ed = Editor::new(grid_doc(1, 1));
        ed.apply_borders(BorderCmd::DiagDown).unwrap();
        assert!(tc(&ed, 0, 0).contains("<w:tl2br w:val=\"single\""));
        assert!(ed.border_state(BorderCmd::DiagDown));
        ed.apply_borders(BorderCmd::DiagUp).unwrap();
        assert!(tc(&ed, 0, 0).contains("<w:tr2bl w:val=\"single\""));
        ed.apply_borders(BorderCmd::DiagDown).unwrap();
        assert!(!tc(&ed, 0, 0).contains("tl2br"));
        ed.apply_borders(BorderCmd::NoBorder).unwrap();
        let x = tc(&ed, 0, 0);
        assert!(!x.contains("tr2bl") && x.contains("<w:top w:val=\"nil\"/>"));
        // Schema order inside tcBorders.
        let i = |tag: &str| x.find(tag).unwrap();
        assert!(
            i("w:top") < i("w:left") && i("w:left") < i("w:bottom") && i("w:bottom") < i("w:right")
        );
    }

    #[test]
    fn design_commands_in_a_nested_table() {
        let mut outer = grid_doc(1, 1);
        let inner = grid_doc(1, 2);
        let Block::Table(tb) = &mut outer.body[0] else {
            panic!()
        };
        tb.rows[0].cells[0].blocks = vec![inner.body[0].clone(), inner.body[1].clone()];
        let mut ed = Editor::new(outer);
        ed.caret = Caret::at(vec![0, 0, 0, 0, 0, 1, 0], 0);
        ed.set_cell_shading(Some("00FF00")).unwrap();
        ed.set_table_style("PlainTable1").unwrap();
        let inner = ed.table(&[0, 0, 0, 0]).unwrap();
        assert!(
            inner.rows[0].cells[1]
                .raw_tcpr
                .as_deref()
                .unwrap()
                .contains("00FF00")
        );
        assert!(inner.raw_tblpr.as_deref().unwrap().contains("PlainTable1"));
        assert!(!t(&ed).raw_tblpr.as_deref().unwrap().contains("PlainTable1"));
    }
}
