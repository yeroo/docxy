//! Table structure helpers shared by the editor's table commands, the
//! terminal editor, the suite and wordcomshim: building a new table, mapping
//! cells onto grid columns (spans, `w:gridBefore`/`w:gridAfter`), reading and
//! writing a cell's `w:tcPr`, and keeping vertical merges consistent.

use crate::model::{Block, Cell, Paragraph, Row, Table, VMerge};
use crate::table_props::{
    PropsXml, TBLPR_ORDER, TCPR_ORDER, row_grid_skips, row_trpr, set_row_trpr,
};

/// How a new table's columns are sized (Insert Table's "AutoFit behaviour").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoFit {
    /// Fixed column width. `None` is Word's "Auto": equal columns over the
    /// text width, with an auto table width.
    Fixed(Option<u32>),
    /// AutoFit to contents.
    Contents,
    /// AutoFit to window.
    Window,
    #[default]
    Default,
}

/// The border set every new table gets (single, auto colour).
const TBL_BORDERS: &str = "<w:tblBorders>\
<w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
</w:tblBorders>";

/// Word's text width for a Letter page with 1" margins, in twips.
pub const DEFAULT_TEXT_WIDTH: u32 = 9360;

/// An empty cell: one blank paragraph and no properties.
pub fn empty_cell() -> Cell {
    Cell {
        blocks: vec![Block::Paragraph(Paragraph::default())],
        ..Cell::default()
    }
}

/// A new `rows`×`cols` table with single borders and equal columns spanning
/// `text_width` twips — the one definition behind Insert Table everywhere.
pub fn new_table(rows: usize, cols: usize, text_width: u32, fit: AutoFit) -> Table {
    let rows = rows.max(1);
    let cols = cols.max(1);
    let col_w = match fit {
        AutoFit::Fixed(Some(w)) => w.max(1),
        _ => text_width.max(cols as u32) / cols as u32,
    };
    let mut tblpr = PropsXml::new("w:tblPr", TBLPR_ORDER);
    tblpr.set(match fit {
        AutoFit::Window => "<w:tblW w:w=\"5000\" w:type=\"pct\"/>",
        _ => "<w:tblW w:w=\"0\" w:type=\"auto\"/>",
    });
    tblpr.set(TBL_BORDERS);
    if let AutoFit::Fixed(Some(_)) = fit {
        tblpr.set("<w:tblLayout w:type=\"fixed\"/>");
    }
    let tcw = match fit {
        AutoFit::Fixed(_) => Some(format!("<w:tcW w:w=\"{col_w}\" w:type=\"dxa\"/>")),
        AutoFit::Contents => Some("<w:tcW w:w=\"0\" w:type=\"auto\"/>".to_string()),
        AutoFit::Window => Some(format!(
            "<w:tcW w:w=\"{}\" w:type=\"pct\"/>",
            5000 / cols as u32
        )),
        AutoFit::Default => None,
    };
    let cell = || {
        let mut c = empty_cell();
        if let Some(w) = &tcw {
            c.raw_tcpr = Some(format!("<w:tcPr>{w}</w:tcPr>"));
        }
        c
    };
    Table {
        grid: vec![col_w; cols],
        rows: (0..rows)
            .map(|_| Row {
                cells: (0..cols).map(|_| cell()).collect(),
                ..Row::default()
            })
            .collect(),
        raw_tblpr: Some(tblpr.to_xml()),
        ..Table::default()
    }
}

// ---- grid map ----

/// Where one row's cells sit on the table grid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RowMap {
    /// `w:gridBefore`: grid columns skipped before the first cell.
    pub before: usize,
    /// `w:gridAfter`: grid columns skipped after the last cell.
    pub after: usize,
    /// Each cell's first grid column and span.
    pub cells: Vec<(usize, usize)>,
}

impl RowMap {
    /// The index of the cell covering grid column `col`.
    pub fn cell_at(&self, col: usize) -> Option<usize> {
        self.cells
            .iter()
            .position(|&(s, n)| col >= s && col < s + n)
    }
    /// The index of the cell starting exactly at grid column `col`.
    pub fn cell_starting(&self, col: usize) -> Option<usize> {
        self.cells.iter().position(|&(s, _)| s == col)
    }
    /// One past the last grid column a cell covers.
    pub fn end(&self) -> usize {
        self.cells.last().map_or(self.before, |&(s, n)| s + n)
    }
}

/// Every row's [`RowMap`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GridMap {
    pub rows: Vec<RowMap>,
}

impl GridMap {
    pub fn of(table: &Table) -> Self {
        GridMap {
            rows: table
                .rows
                .iter()
                .map(|row| {
                    let (before, after) = row_grid_skips(&row.raw_props);
                    let mut col = before;
                    let cells = row
                        .cells
                        .iter()
                        .map(|c| {
                            let n = c.grid_span.max(1) as usize;
                            let s = col;
                            col += n;
                            (s, n)
                        })
                        .collect();
                    RowMap {
                        before,
                        after,
                        cells,
                    }
                })
                .collect(),
        }
    }

    /// The number of grid columns: the table grid, or the widest row when the
    /// grid is shorter.
    pub fn width(&self, table: &Table) -> usize {
        self.rows
            .iter()
            .map(|r| r.end() + r.after)
            .max()
            .unwrap_or(0)
            .max(table.grid.len())
    }

    /// `(start, span)` of cell `(row, cell)`.
    pub fn span(&self, row: usize, cell: usize) -> Option<(usize, usize)> {
        self.rows.get(row)?.cells.get(cell).copied()
    }

    /// The cell a vertically merged cell belongs to: walk up from a `continue`
    /// cell to its `restart` cell at the same grid column.
    pub fn owner(&self, table: &Table, row: usize, cell: usize) -> (usize, usize) {
        let (mut r, mut c) = (row, cell);
        while r > 0 && table.rows[r].cells.get(c).map(|x| x.v_merge) == Some(VMerge::Continue) {
            let Some((start, _)) = self.span(r, c) else {
                break;
            };
            let Some(above) = self.rows[r - 1].cell_starting(start) else {
                break;
            };
            r -= 1;
            c = above;
        }
        (r, c)
    }

    /// The last row of the vertical merge that starts at `(row, cell)`.
    pub fn merge_end(&self, table: &Table, row: usize, cell: usize) -> usize {
        let Some((start, _)) = self.span(row, cell) else {
            return row;
        };
        let mut r = row;
        while let Some(below) = self.rows.get(r + 1).and_then(|m| m.cell_starting(start)) {
            if table.rows[r + 1].cells[below].v_merge != VMerge::Continue {
                break;
            }
            r += 1;
        }
        r
    }
}

// ---- cell properties ----

/// A cell's `w:tcPr`, parsed. The model's `grid_span`/`v_merge` stay the
/// source of truth; the serializer writes them over whatever this holds.
pub fn cell_props(cell: &Cell) -> PropsXml {
    PropsXml::parse_opt(cell.raw_tcpr.as_deref(), "w:tcPr", TCPR_ORDER)
}

/// Store `props` as the cell's `w:tcPr`. A container holding nothing the model
/// does not already describe (only `gridSpan`/`vMerge`) is dropped, like a
/// loaded cell's.
pub fn set_cell_props(cell: &mut Cell, props: &PropsXml) {
    let extra = props
        .child_names()
        .iter()
        .any(|n| n != "w:gridSpan" && n != "w:vMerge");
    cell.raw_tcpr = (extra || cell.property_change.is_some()).then(|| props.to_xml());
}

/// Edit a cell's `w:tcPr` in place.
pub fn edit_cell_props(cell: &mut Cell, edit: impl FnOnce(&mut PropsXml)) {
    let mut p = cell_props(cell);
    edit(&mut p);
    set_cell_props(cell, &p);
}

/// A table's `w:tblPr`, parsed.
pub fn table_props(table: &Table) -> PropsXml {
    PropsXml::parse_opt(table.raw_tblpr.as_deref(), "w:tblPr", TBLPR_ORDER)
}

/// Edit a table's `w:tblPr` in place.
pub fn edit_table_props(table: &mut Table, edit: impl FnOnce(&mut PropsXml)) {
    let mut p = table_props(table);
    edit(&mut p);
    table.raw_tblpr = (!p.is_empty() || table.property_change.is_some()).then(|| p.to_xml());
}

/// Set a row's `w:gridBefore`/`w:gridAfter` (0 removes it).
pub fn set_row_skips(row: &mut Row, before: usize, after: usize) {
    let mut t = row_trpr(&row.raw_props);
    for (name, n) in [("w:gridBefore", before), ("w:gridAfter", after)] {
        if n == 0 {
            t.remove(name);
        } else {
            t.set(&format!("<{name} w:val=\"{n}\"/>"));
        }
    }
    set_row_trpr(&mut row.raw_props, &t);
}

/// A copy of `cell` as the template for a new cell: its formatting without
/// vertical merge, revision marks or content (one empty paragraph keeping the
/// first paragraph's properties).
pub fn template_cell(cell: &Cell) -> Cell {
    let mut props = cell_props(cell);
    for name in [
        "w:vMerge",
        "w:hMerge",
        "w:cellIns",
        "w:cellDel",
        "w:cellMerge",
        "w:tcPrChange",
    ] {
        props.remove(name);
    }
    let para = match cell.blocks.first() {
        Some(Block::Paragraph(p)) => {
            let mut props = p.props.clone();
            props.property_change = None;
            props.section_property_change = None;
            props.section_break = None;
            Paragraph {
                props,
                content: Vec::new(),
            }
        }
        _ => Paragraph::default(),
    };
    let mut out = Cell {
        grid_span: cell.grid_span.max(1),
        v_merge: VMerge::None,
        blocks: vec![Block::Paragraph(para)],
        raw_tcpr: None,
        property_change: None,
        unsupported_revisions: Vec::new(),
    };
    set_cell_props(&mut out, &props);
    out
}

/// A copy of `row` as the template for a new row: each cell through
/// [`template_cell`], row properties without revision marks.
pub fn template_row(row: &Row) -> Row {
    let raw_props = row
        .raw_props
        .iter()
        .filter_map(|raw| {
            if crate::table_props::element_name(raw) == "w:trPr" {
                let mut t = row_trpr(std::slice::from_ref(raw));
                for name in ["w:ins", "w:del", "w:trPrChange"] {
                    t.remove(name);
                }
                (!t.is_empty()).then(|| t.to_xml())
            } else {
                Some(raw.clone())
            }
        })
        .collect();
    Row {
        cells: row.cells.iter().map(template_cell).collect(),
        raw_props,
        property_change: None,
    }
}

/// Repair vertical merges after rows or cells moved: a `continue` cell with no
/// merged cell above it at the same columns becomes a `restart`, and a
/// `restart` with nothing continuing it below becomes unmerged.
pub fn normalize_vmerge(table: &mut Table) {
    let map = GridMap::of(table);
    for r in 0..table.rows.len() {
        for c in 0..table.rows[r].cells.len() {
            if table.rows[r].cells[c].v_merge != VMerge::Continue {
                continue;
            }
            let (s, n) = map.rows[r].cells[c];
            let above = (r > 0)
                .then(|| map.rows[r - 1].cell_starting(s))
                .flatten()
                .filter(|&a| {
                    map.rows[r - 1].cells[a].1 == n
                        && table.rows[r - 1].cells[a].v_merge != VMerge::None
                });
            if above.is_none() {
                table.rows[r].cells[c].v_merge = VMerge::Restart;
            }
        }
    }
    for r in 0..table.rows.len() {
        for c in 0..table.rows[r].cells.len() {
            if table.rows[r].cells[c].v_merge != VMerge::Restart {
                continue;
            }
            let (s, n) = map.rows[r].cells[c];
            let continued = map
                .rows
                .get(r + 1)
                .and_then(|m| m.cell_starting(s).filter(|&b| m.cells[b].1 == n))
                .is_some_and(|b| table.rows[r + 1].cells[b].v_merge == VMerge::Continue);
            if !continued {
                table.rows[r].cells[c].v_merge = VMerge::None;
            }
        }
    }
}

/// Cut the grid at the given x positions (twips from the table's left edge),
/// re-spanning every cell so nothing moves. Returns, for each old grid column
/// boundary index `0..=len`, its new index.
pub fn refine_grid(table: &mut Table, cuts: &[u32]) -> Vec<usize> {
    let mut xs: Vec<u32> = vec![0];
    let mut acc = 0u32;
    for w in &table.grid {
        acc += w;
        xs.push(acc);
    }
    let mut all = xs.clone();
    for &c in cuts {
        if c > 0 && c < acc {
            all.push(c);
        }
    }
    all.sort_unstable();
    all.dedup();
    let new_index: Vec<usize> = xs
        .iter()
        .map(|x| all.iter().position(|a| a == x).unwrap_or(0))
        .collect();
    table.grid = all.windows(2).map(|w| w[1] - w[0]).collect();
    let map_col = |old: usize| new_index[old.min(new_index.len() - 1)];
    for row in &mut table.rows {
        let (before, after) = row_grid_skips(&row.raw_props);
        let mut col = before;
        for cell in &mut row.cells {
            let n = cell.grid_span.max(1) as usize;
            let (a, b) = (map_col(col), map_col(col + n));
            cell.grid_span = (b - a).max(1) as u32;
            col += n;
        }
        let end = col;
        let total = new_index.len() - 1;
        let new_before = map_col(before);
        let new_after = if after > 0 {
            map_col((end + after).min(total)) - map_col(end)
        } else {
            0
        };
        if new_before != before || new_after != after {
            set_row_skips(row, new_before, new_after);
        }
    }
    new_index
}

/// The x offset (twips) of each grid column boundary.
pub fn grid_xs(table: &Table) -> Vec<u32> {
    let mut xs = vec![0];
    let mut acc = 0;
    for w in &table.grid {
        acc += w;
        xs.push(acc);
    }
    xs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_table_default_keeps_the_classic_markup() {
        let t = new_table(2, 3, 9360, AutoFit::Default);
        assert_eq!(t.grid, vec![3120; 3]);
        assert_eq!(t.rows.len(), 2);
        assert!(t.rows.iter().all(|r| r.cells.len() == 3));
        let tblpr = t.raw_tblpr.unwrap();
        assert!(tblpr.starts_with("<w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders>"));
        assert!(t.rows[0].cells[0].raw_tcpr.is_none());
    }

    #[test]
    fn new_table_autofit_modes() {
        let fixed = new_table(1, 2, 9360, AutoFit::Fixed(Some(1440)));
        assert_eq!(fixed.grid, vec![1440, 1440]);
        assert!(
            fixed
                .raw_tblpr
                .as_deref()
                .unwrap()
                .contains("<w:tblLayout w:type=\"fixed\"/>")
        );
        assert!(
            fixed.rows[0].cells[0]
                .raw_tcpr
                .as_deref()
                .unwrap()
                .contains("w:w=\"1440\" w:type=\"dxa\"")
        );
        let window = new_table(1, 2, 9360, AutoFit::Window);
        assert!(
            window
                .raw_tblpr
                .as_deref()
                .unwrap()
                .contains("w:w=\"5000\" w:type=\"pct\"")
        );
        assert!(
            window.rows[0].cells[0]
                .raw_tcpr
                .as_deref()
                .unwrap()
                .contains("w:w=\"2500\" w:type=\"pct\"")
        );
        let contents = new_table(1, 2, 9360, AutoFit::Contents);
        assert!(
            contents.rows[0].cells[0]
                .raw_tcpr
                .as_deref()
                .unwrap()
                .contains("w:type=\"auto\"")
        );
    }

    fn row(spans: &[u32], trpr: Option<&str>) -> Row {
        Row {
            cells: spans
                .iter()
                .map(|&n| Cell {
                    grid_span: n,
                    ..empty_cell()
                })
                .collect(),
            raw_props: trpr.map(|t| vec![t.to_string()]).unwrap_or_default(),
            property_change: None,
        }
    }

    #[test]
    fn grid_map_honours_spans_and_grid_before() {
        let t = Table {
            grid: vec![100; 4],
            rows: vec![
                row(&[1, 2, 1], None),
                row(
                    &[1, 2],
                    Some("<w:trPr><w:gridBefore w:val=\"1\"/></w:trPr>"),
                ),
            ],
            ..Table::default()
        };
        let m = GridMap::of(&t);
        assert_eq!(m.rows[0].cells, vec![(0, 1), (1, 2), (3, 1)]);
        assert_eq!(m.rows[1].cells, vec![(1, 1), (2, 2)]);
        assert_eq!(m.rows[1].cell_at(0), None);
        assert_eq!(m.rows[1].cell_at(3), Some(1));
        assert_eq!(m.width(&t), 4);
    }

    #[test]
    fn refine_grid_keeps_every_cell_in_place() {
        let mut t = Table {
            grid: vec![1000, 1000],
            rows: vec![row(&[1, 1], None), row(&[2], None)],
            ..Table::default()
        };
        let idx = refine_grid(&mut t, &[500]);
        assert_eq!(t.grid, vec![500, 500, 1000]);
        assert_eq!(idx, vec![0, 2, 3]);
        assert_eq!(t.rows[0].cells[0].grid_span, 2);
        assert_eq!(t.rows[0].cells[1].grid_span, 1);
        assert_eq!(t.rows[1].cells[0].grid_span, 3);
    }

    #[test]
    fn normalize_vmerge_repairs_orphans() {
        let mut t = Table {
            grid: vec![100],
            rows: vec![row(&[1], None), row(&[1], None), row(&[1], None)],
            ..Table::default()
        };
        t.rows[0].cells[0].v_merge = VMerge::Restart;
        t.rows[2].cells[0].v_merge = VMerge::Continue;
        normalize_vmerge(&mut t);
        assert_eq!(t.rows[0].cells[0].v_merge, VMerge::None);
        assert_eq!(t.rows[2].cells[0].v_merge, VMerge::None);
    }

    #[test]
    fn template_row_drops_merge_and_revisions() {
        let mut r = row(
            &[2],
            Some("<w:trPr><w:trHeight w:val=\"400\"/><w:ins w:id=\"1\"/></w:trPr>"),
        );
        r.cells[0].v_merge = VMerge::Restart;
        r.cells[0].raw_tcpr = Some(
            "<w:tcPr><w:gridSpan w:val=\"2\"/><w:vMerge w:val=\"restart\"/><w:shd w:fill=\"FF0000\"/><w:cellIns w:id=\"2\"/></w:tcPr>".into(),
        );
        let t = template_row(&r);
        assert_eq!(
            t.raw_props,
            vec!["<w:trPr><w:trHeight w:val=\"400\"/></w:trPr>".to_string()]
        );
        let c = &t.cells[0];
        assert_eq!((c.grid_span, c.v_merge), (2, VMerge::None));
        assert_eq!(
            c.raw_tcpr.as_deref(),
            Some("<w:tcPr><w:gridSpan w:val=\"2\"/><w:shd w:fill=\"FF0000\"/></w:tcPr>")
        );
    }
}
