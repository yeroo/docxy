//! Paste Special, as Excel has it (#669): what part of a copy is pasted
//! (all, formulas, values, formats, notes, validation, column widths, and
//! their combinations), an arithmetic operation with the destination, skip
//! blanks, transpose, and Paste Link; and the shape rules of a multi-area
//! copy (#670).
//!
//! A copy is a [`ClipBlock`]: its cells, and for each block row and column
//! the sheet row and column it came from. Formulas are translated cell by
//! cell from where they were copied to where they land, so a filtered copy,
//! a multi-area copy and a transposed paste all read relative to their own
//! cells.

use crate::formula::{Expr, Transposed, to_string, translate_formula, transpose_formula};
use crate::sheet::{Cell, CellValue, MAX_COLS, MAX_ROWS, Sheet, Workbook, cell_name};

use super::{Area, rects_overlap as overlaps};

/// Why a block whose rows and columns no longer cover its cells is refused
/// ([`ClipBlock::is_consistent`]).
pub(crate) const BLOCK_STALE: &str = "The copy no longer matches its cells: copy again";

/// Why a transposed paste of a spilling array is refused (#707 r5 m1).
pub(crate) const TRANSPOSE_ARRAY: &str =
    "You can't transpose part of an array: paste its values instead.";

/// Excel's refusal of a multi-area copy whose areas share neither their rows
/// nor their columns, of any multi-area cut, and of a paste or a drag over a
/// multi-area selection.
pub const MULTI_SELECTION: &str = "This action won't work on multiple selections.";

/// What a Paste Special pastes: the dialog's Paste group, and the gallery's
/// combinations of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PasteWhat {
    /// Contents, formats, validation and notes.
    #[default]
    All,
    /// Formulas (and constants), in the destination's formats.
    Formulas,
    /// Values (a formula's result), in the destination's formats.
    Values,
    /// Formats only.
    Formats,
    /// Notes (Comments and Notes).
    Comments,
    /// Data validation only.
    Validation,
    /// All, with the borders taken off the pasted styles (No Borders).
    AllExceptBorders,
    /// The source's column widths only.
    ColumnWidths,
    /// Formulas, and the source's number formats.
    FormulasAndNumberFormats,
    /// Values, and the source's number formats.
    ValuesAndNumberFormats,
    /// All, and the source's column widths (Keep Source Column Widths).
    AllAndColumnWidths,
    /// Values in the source's formats (Values & Source Formatting).
    ValuesAndSourceFormatting,
}

impl PasteWhat {
    /// The dialog's Paste options, in its order.
    pub const DIALOG: [PasteWhat; 10] = [
        PasteWhat::All,
        PasteWhat::Formulas,
        PasteWhat::Values,
        PasteWhat::Formats,
        PasteWhat::Comments,
        PasteWhat::Validation,
        PasteWhat::AllExceptBorders,
        PasteWhat::ColumnWidths,
        PasteWhat::FormulasAndNumberFormats,
        PasteWhat::ValuesAndNumberFormats,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PasteWhat::All => "All",
            PasteWhat::Formulas => "Formulas",
            PasteWhat::Values => "Values",
            PasteWhat::Formats => "Formats",
            PasteWhat::Comments => "Comments and Notes",
            PasteWhat::Validation => "Validation",
            PasteWhat::AllExceptBorders => "All except borders",
            PasteWhat::ColumnWidths => "Column widths",
            PasteWhat::FormulasAndNumberFormats => "Formulas and number formats",
            PasteWhat::ValuesAndNumberFormats => "Values and number formats",
            PasteWhat::AllAndColumnWidths => "Keep Source Column Widths",
            PasteWhat::ValuesAndSourceFormatting => "Values & Source Formatting",
        }
    }

    /// The option a label names, any case; also `comments`, `notes`.
    #[cfg(test)]
    pub fn from_label(s: &str) -> Option<PasteWhat> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("comments") || s.eq_ignore_ascii_case("notes") {
            return Some(PasteWhat::Comments);
        }
        PasteWhat::DIALOG
            .into_iter()
            .chain([
                PasteWhat::AllAndColumnWidths,
                PasteWhat::ValuesAndSourceFormatting,
            ])
            .find(|w| w.label().eq_ignore_ascii_case(s))
    }

    /// Whether it pastes what the cells show, never their formulas.
    pub fn values_only(self) -> bool {
        matches!(
            self,
            PasteWhat::Values
                | PasteWhat::ValuesAndNumberFormats
                | PasteWhat::ValuesAndSourceFormatting
        )
    }

    /// Whether it pastes cell contents (and so takes an operation).
    pub fn pastes_contents(self) -> bool {
        !matches!(
            self,
            PasteWhat::Formats
                | PasteWhat::Comments
                | PasteWhat::Validation
                | PasteWhat::ColumnWidths
        )
    }

    fn all(self) -> bool {
        matches!(
            self,
            PasteWhat::All | PasteWhat::AllExceptBorders | PasteWhat::AllAndColumnWidths
        )
    }
}

/// The Operation a Paste Special applies between the copy and the
/// destination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PasteOp {
    #[default]
    None,
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl PasteOp {
    pub const ALL: [PasteOp; 5] = [
        PasteOp::None,
        PasteOp::Add,
        PasteOp::Subtract,
        PasteOp::Multiply,
        PasteOp::Divide,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PasteOp::None => "None",
            PasteOp::Add => "Add",
            PasteOp::Subtract => "Subtract",
            PasteOp::Multiply => "Multiply",
            PasteOp::Divide => "Divide",
        }
    }

    #[cfg(test)]
    pub fn from_label(s: &str) -> Option<PasteOp> {
        PasteOp::ALL
            .into_iter()
            .find(|o| o.label().eq_ignore_ascii_case(s.trim()))
    }

    fn sign(self) -> &'static str {
        match self {
            PasteOp::None => "",
            PasteOp::Add => "+",
            PasteOp::Subtract => "-",
            PasteOp::Multiply => "*",
            PasteOp::Divide => "/",
        }
    }
}

/// The Paste Special dialog's choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PasteSpec {
    pub what: PasteWhat,
    pub op: PasteOp,
    /// A blank copied cell leaves its destination alone.
    pub skip_blanks: bool,
    /// Rows become columns.
    pub transpose: bool,
}

impl PasteSpec {
    /// The spec that pastes `what` and nothing else special.
    pub fn of(what: PasteWhat) -> Self {
        PasteSpec {
            what,
            ..PasteSpec::default()
        }
    }
}

/// A note on a copied cell, at its block position.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipNote {
    pub at: (usize, usize),
    pub author: String,
    pub text: String,
}

/// A data-validation rule over some of the copied cells (block positions).
#[derive(Clone, Debug, PartialEq)]
pub struct ClipRule {
    pub kind: String,
    pub operator: String,
    pub formula1: String,
    pub formula2: String,
    pub prompt: Option<String>,
    pub cells: Vec<(usize, usize)>,
    /// The source rule's anchor (the top-left of its ranges, as
    /// `shift_rule_ranges` reads it): its formulas are relative to it.
    pub anchor: (u32, u32),
}

/// The positions in sorted `v` of the values `lo..=hi`.
fn span(v: &[u32], lo: u32, hi: u32) -> std::ops::Range<usize> {
    v.partition_point(|&x| x < lo)..v.partition_point(|&x| x <= hi)
}

/// A copy, as Paste Special reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipBlock {
    /// The copied cells, a row per copied row.
    pub cells: Vec<Vec<Cell>>,
    /// The sheet row each block row came from.
    pub rows: Vec<u32>,
    /// The sheet column each block column came from.
    pub cols: Vec<u32>,
    /// The sheet it came from, and that sheet's name.
    pub sheet: usize,
    pub sheet_name: String,
    /// Each block column's width on its sheet, in characters.
    pub widths: Vec<f64>,
    /// The notes on the copied cells. Notes live in the package, so the host
    /// fills them in ([`ClipBlock::set_notes`]).
    pub notes: Vec<ClipNote>,
    /// The data validation over the copied cells.
    pub rules: Vec<ClipRule>,
    /// Clipboard text from another program, each field read as typed where
    /// it lands (`rows`/`cols` are those destinations): a formula in it is
    /// never moved, even transposed, but kept as typed at its destination,
    /// and Values evaluates it there (#707 r8 M2).
    pub typed: bool,
}

impl ClipBlock {
    /// The cells of `rows` × `cols` on `sheet`, with their column widths and
    /// the validation over them. `rows` and `cols` are in sheet order.
    pub fn capture(wb: &Workbook, sheet: usize, rows: Vec<u32>, cols: Vec<u32>) -> ClipBlock {
        let Some(s) = wb.sheets.get(sheet) else {
            return ClipBlock::default();
        };
        let cells = rows
            .iter()
            .map(|&r| {
                cols.iter()
                    .map(|&c| s.cell(r, c).cloned().unwrap_or_default())
                    .collect()
            })
            .collect();
        let widths = cols.iter().map(|&c| s.col_width(c)).collect();
        let rules = s
            .validations
            .iter()
            .filter_map(|dv| {
                // Each range's cells in the copy, found by binary search in
                // the sorted rows and columns: a rule off the copy costs
                // nothing (#707 r6).
                let mut cells = Vec::new();
                for &(ra, ca, rb, cb) in &dv.ranges {
                    for i in span(&rows, ra, rb) {
                        for j in span(&cols, ca, cb) {
                            cells.push((i, j));
                        }
                    }
                }
                if dv.ranges.len() > 1 {
                    cells.sort_unstable();
                    cells.dedup();
                }
                let anchor = rule_anchor(&dv.ranges);
                (!cells.is_empty()).then(|| ClipRule {
                    anchor,
                    kind: dv.kind.clone(),
                    operator: dv.operator.clone(),
                    formula1: dv.formula1.clone(),
                    formula2: dv.formula2.clone(),
                    prompt: dv.prompt.clone(),
                    cells,
                })
            })
            .collect();
        ClipBlock {
            cells,
            rows,
            cols,
            sheet,
            sheet_name: s.name.clone(),
            widths,
            notes: Vec::new(),
            rules,
            typed: false,
        }
    }

    /// The notes among `notes` — `(row, col, author, text)` on the copy's
    /// sheet — that sit on copied cells.
    pub fn set_notes(&mut self, notes: impl IntoIterator<Item = (u32, u32, String, String)>) {
        self.notes = notes
            .into_iter()
            .filter_map(|(r, c, author, text)| {
                let i = self.rows.binary_search(&r).ok()?;
                let j = self.cols.binary_search(&c).ok()?;
                Some(ClipNote {
                    at: (i, j),
                    author,
                    text,
                })
            })
            .collect();
    }

    /// Whether every copied cell has its source row and column: a block
    /// whose `cells` outgrew `rows`/`cols` (its sheet gone, say, so
    /// [`ClipBlock::capture`] found nothing) is refused by
    /// [`paste_special_changes`] (#707 r1, r8).
    pub(crate) fn is_consistent(&self) -> bool {
        self.cells.len() == self.rows.len()
            && self.cells.iter().all(|row| row.len() <= self.cols.len())
    }

    /// The block positions an array formula of the copy spills into (its
    /// own cell aside): a dynamic array's spill, or a legacy CSE block.
    fn spill_members(&self) -> std::collections::HashSet<(usize, usize)> {
        // The block's rows and columns are in sheet order, so each anchor's
        // extent is found by binary search: only its own cells are visited
        // (#707 r6 M3).
        let span = |v: &[u32], from: u32, len: u32| {
            let lo = v.partition_point(|&x| x < from);
            let hi = v.partition_point(|&x| u64::from(x) < u64::from(from) + u64::from(len));
            lo..hi
        };
        let mut members = std::collections::HashSet::new();
        for (i, row) in self.cells.iter().enumerate() {
            for (j, cell) in row.iter().enumerate() {
                let (Some((h, w)), Some(_), Some(&r0), Some(&c0)) = (
                    cell.spill,
                    &cell.formula,
                    self.rows.get(i),
                    self.cols.get(j),
                ) else {
                    continue;
                };
                for ii in span(&self.rows, r0, h) {
                    for jj in span(&self.cols, c0, w) {
                        if (ii, jj) != (i, j) {
                            members.insert((ii, jj));
                        }
                    }
                }
            }
        }
        members
    }

    /// (rows, cols) of the copy.
    pub fn size(&self) -> (u32, u32) {
        (self.rows.len() as u32, self.cols.len() as u32)
    }

    /// (rows, cols) it covers when pasted, `transpose`d or not.
    pub fn pasted_size(&self, transpose: bool) -> (u32, u32) {
        let (h, w) = self.size();
        if transpose { (w, h) } else { (h, w) }
    }

    /// Where block cell `(i, j)` lands when pasted at `at`; `None` off the
    /// grid.
    fn dest(&self, at: (u32, u32), (i, j): (usize, usize), transpose: bool) -> Option<(u32, u32)> {
        let (di, dj) = if transpose { (j, i) } else { (i, j) };
        let r = u64::from(at.0) + di as u64;
        let c = u64::from(at.1) + dj as u64;
        (r < u64::from(MAX_ROWS) && c < u64::from(MAX_COLS)).then_some((r as u32, c as u32))
    }

    /// The rectangle a paste at `at` covers, cut at the grid's edge.
    pub fn pasted_rect(&self, at: (u32, u32), transpose: bool) -> Area {
        let (h, w) = self.pasted_size(transpose);
        let r1 = (u64::from(at.0) + u64::from(h.max(1)) - 1).min(u64::from(MAX_ROWS - 1));
        let c1 = (u64::from(at.1) + u64::from(w.max(1)) - 1).min(u64::from(MAX_COLS - 1));
        (at.0, at.1, r1 as u32, c1 as u32)
    }

    /// The formula of block cell `(i, j)` as it reads pasted at `dest`.
    fn moved_formula(
        &self,
        f: &str,
        (i, j): (usize, usize),
        dest: (u32, u32),
        at: (u32, u32),
        transpose: bool,
        dst_sheet: &str,
    ) -> Option<String> {
        let dr = i64::from(dest.0) - i64::from(*self.rows.get(i)?);
        let dc = i64::from(dest.1) - i64::from(*self.cols.get(j)?);
        if transpose {
            let inside = |row: i64, col: i64| -> Option<(i64, i64)> {
                let i = self.rows.binary_search(&u32::try_from(row).ok()?).ok()?;
                let j = self.cols.binary_search(&u32::try_from(col).ok()?).ok()?;
                Some((i64::from(at.0) + j as i64, i64::from(at.1) + i as i64))
            };
            let t = Transposed {
                src_sheet: &self.sheet_name,
                dst_sheet,
                inside: &inside,
                dr,
                dc,
            };
            transpose_formula(f, &t)
        } else if (dr, dc) == (0, 0) {
            None
        } else {
            translate_formula(f, dr, dc)
        }
    }
}

/// The rows and columns a multi-area copy of `areas` takes, as one block in
/// sheet order: allowed only when every area spans the same rows (the
/// columns are joined) or the same columns (the rows are joined), as Excel
/// has it; otherwise [`MULTI_SELECTION`].
pub fn multi_area_shape(areas: &[Area]) -> Result<(Vec<u32>, Vec<u32>), &'static str> {
    let Some(&(r0, c0, r1, c1)) = areas.first() else {
        return Err(MULTI_SELECTION);
    };
    let join = |spans: Vec<(u32, u32)>| {
        let mut v: Vec<u32> = spans.into_iter().flat_map(|(a, b)| a..=b).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    if areas.iter().all(|a| (a.0, a.2) == (r0, r1)) {
        let cols = join(areas.iter().map(|a| (a.1, a.3)).collect());
        return Ok(((r0..=r1).collect(), cols));
    }
    if areas.iter().all(|a| (a.1, a.3) == (c0, c1)) {
        let rows = join(areas.iter().map(|a| (a.0, a.2)).collect());
        return Ok((rows, (c0..=c1).collect()));
    }
    Err(MULTI_SELECTION)
}

/// One side of a paste operation.
enum Operand {
    Num(f64),
    Blank,
    Formula(String),
    /// Text, a logical or an error: the operation leaves the cell alone.
    Other,
}

fn operand_of(cell: &Cell, formula: Option<String>) -> Operand {
    if let Some(f) = formula {
        return Operand::Formula(f);
    }
    match &cell.value {
        CellValue::Number(n) => Operand::Num(*n),
        CellValue::Empty => Operand::Blank,
        _ => Operand::Other,
    }
}

fn num_text(n: f64) -> String {
    to_string(&Expr::Num(n))
}

/// `d op s`, as Excel's Paste Special operations write it: two numbers
/// (a blank is 0) give the number, `#DIV/0!` for a division by zero; a
/// formula on either side gives a formula, each formula's text bracketed:
/// `=(B9*2)*1.05`, `=10+(A1)`, `=0+(A1)`. Text, a logical or an error on
/// either side leaves the destination alone (`None`).
fn apply_op(op: PasteOp, d: Operand, s: Operand) -> Option<Result<f64, String>> {
    let side = |o: &Operand, formula_paren: bool| match o {
        Operand::Num(n) => Some(num_text(*n)),
        Operand::Blank => Some("0".to_string()),
        Operand::Formula(f) if formula_paren => Some(format!("({f})")),
        Operand::Formula(f) => Some(f.clone()),
        Operand::Other => None,
    };
    match (&d, &s) {
        (Operand::Other, _) | (_, Operand::Other) => None,
        (Operand::Formula(_), _) | (_, Operand::Formula(_)) => Some(Err(format!(
            "{}{}{}",
            side(&d, true)?,
            op.sign(),
            side(&s, true)?
        ))),
        _ => {
            let n = |o: &Operand| match o {
                Operand::Num(n) => *n,
                _ => 0.0,
            };
            let (a, b) = (n(&d), n(&s));
            Some(Ok(match op {
                PasteOp::None => b,
                PasteOp::Add => a + b,
                PasteOp::Subtract => a - b,
                PasteOp::Multiply => a * b,
                PasteOp::Divide if b == 0.0 => f64::NAN,
                PasteOp::Divide => a / b,
            }))
        }
    }
}

/// The `(row, col, cell)` writes a Paste Special of `clip` at `at` on
/// `sheet` makes (styles it needs are interned into `wb`'s styles). Notes,
/// validation and column widths are not cells: [`paste_special_extras`].
pub fn paste_special_changes(
    wb: &mut Workbook,
    sheet: usize,
    at: (u32, u32),
    clip: &ClipBlock,
    spec: &PasteSpec,
) -> Result<Vec<(u32, u32, Cell)>, &'static str> {
    if !clip.is_consistent() {
        return Err(BLOCK_STALE);
    }
    let mut out = Vec::new();
    if !spec.what.pastes_contents() && spec.what != PasteWhat::Formats {
        return Ok(out);
    }
    // A typed formula pasted as a value: what it evaluates to where it
    // lands, as the sheet stands before the paste.
    let values_only = spec.what.values_only();
    let mut typed_values: std::collections::HashMap<(usize, usize), CellValue> = Default::default();
    if clip.typed && values_only && sheet < wb.sheets.len() {
        for (i, row) in clip.cells.iter().enumerate().take(clip.rows.len()) {
            for (j, src) in row.iter().enumerate().take(clip.cols.len()) {
                let (Some(f), Some(dest)) = (
                    src.formula.as_deref(),
                    clip.dest(at, (i, j), spec.transpose),
                ) else {
                    continue;
                };
                let v = crate::engine::eval_formula_at(wb, sheet, dest.0, dest.1, f);
                typed_values.insert((i, j), crate::engine::value_to_cell(v));
            }
        }
    }
    let Workbook { sheets, styles, .. } = wb;
    let Some(s) = sheets.get(sheet) else {
        return Ok(out);
    };
    let members = clip.spill_members();
    // A transposed array formula would spill along the wrong axis over cells
    // the paste never cleared: refused, as Excel refuses changing part of an
    // array (#707 r5 m1). Values transpose what the cells show.
    if spec.transpose && !values_only && !members.is_empty() {
        return Err(TRANSPOSE_ARRAY);
    }
    // The styles a paste mixes, each interned once per (source, destination)
    // pair: interning scans the style table (#707 r6).
    let mut mixed: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();
    // Only the cells that know where they came from.
    for (i, row) in clip.cells.iter().enumerate().take(clip.rows.len()) {
        for (j, src) in row.iter().enumerate().take(clip.cols.len()) {
            let Some(dest) = clip.dest(at, (i, j), spec.transpose) else {
                continue;
            };
            if spec.skip_blanks && src.is_blank() {
                continue;
            }
            let d = s.cell(dest.0, dest.1).cloned().unwrap_or_default();
            let style = match spec.what {
                PasteWhat::All
                | PasteWhat::AllAndColumnWidths
                | PasteWhat::Formats
                | PasteWhat::ValuesAndSourceFormatting => src.style,
                PasteWhat::AllExceptBorders => *mixed.entry((src.style, 0)).or_insert_with(|| {
                    let mut xf = styles.xf(src.style);
                    xf.border = false;
                    styles.intern(xf)
                }),
                PasteWhat::FormulasAndNumberFormats | PasteWhat::ValuesAndNumberFormats => {
                    *mixed.entry((src.style, d.style)).or_insert_with(|| {
                        let from = styles.xf(src.style);
                        let mut xf = styles.xf(d.style);
                        xf.numfmt = from.numfmt;
                        xf.code = from.code;
                        styles.intern(xf)
                    })
                }
                _ => d.style,
            };
            if spec.what == PasteWhat::Formats {
                if style != d.style || s.cell(dest.0, dest.1).is_none() {
                    out.push((dest.0, dest.1, Cell { style, ..d }));
                }
                continue;
            }
            let formula = if values_only {
                None
            } else if clip.typed {
                src.formula.clone()
            } else {
                src.formula.as_deref().map(|f| {
                    clip.moved_formula(f, (i, j), dest, at, spec.transpose, &s.name)
                        .unwrap_or_else(|| f.to_string())
                })
            };
            // A cell an array formula of the copy spills into is left blank
            // where that formula is pasted with it: the pasted formula
            // spills there again, where a constant would block it (#707 r4
            // M1), with an operation too (r5 m1). A paste of values writes what the cells show.
            if !values_only && members.contains(&(i, j)) {
                out.push((
                    dest.0,
                    dest.1,
                    Cell {
                        style,
                        ..Cell::default()
                    },
                ));
                continue;
            }
            if spec.op != PasteOp::None {
                let dv = operand_of(&d, d.formula.clone());
                let sv = operand_of(src, formula);
                let Some(res) = apply_op(spec.op, dv, sv) else {
                    continue;
                };
                let mut cell = match res {
                    Ok(n) if n.is_nan() => Cell {
                        value: CellValue::Error("#DIV/0!".into()),
                        ..Cell::default()
                    },
                    Ok(n) => Cell::number(n),
                    Err(f) => {
                        // Typed here, as a filled formula is: an array one
                        // spills where it lands.
                        let mut c = Cell::formula(&f);
                        super::rebase(&mut c, 0, 0);
                        c
                    }
                };
                cell.style = style;
                out.push((dest.0, dest.1, cell));
                continue;
            }
            let mut cell = match formula {
                Some(f) => {
                    let mut c = src.clone();
                    c.formula = Some(f);
                    // An array's `<f>` attributes name the source's block
                    // (a legacy CSE `ref`): the copy is a formula of its own
                    // that spills where it lands, as a filled one is.
                    super::rebase(&mut c, 0, 0);
                    c
                }
                None if values_only || !spec.what.all() => Cell {
                    value: typed_values
                        .remove(&(i, j))
                        .unwrap_or_else(|| src.value.clone()),
                    ..Cell::default()
                },
                None => src.clone(),
            };
            cell.style = style;
            out.push((dest.0, dest.1, cell));
        }
    }
    Ok(out)
}

/// The parts of a Paste Special that are not cell writes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PasteExtras {
    /// Notes to write: (row, col, author, text).
    pub notes: Vec<(u32, u32, String, String)>,
    /// Validation to clear from these cells first, then rules to add, one
    /// rectangle each.
    pub clear_rules: Option<Area>,
    pub rules: Vec<(Area, ClipRule)>,
    /// Column widths to set: (column, width in characters).
    pub widths: Vec<(u32, f64)>,
}

/// What else a Paste Special of `clip` at `at` writes: notes (Comments and
/// Notes, and All), validation (Validation, and All), and column widths
/// (Column widths, Keep Source Column Widths). The host writes notes and
/// validation rules through its package, which holds them.
pub fn paste_special_extras(clip: &ClipBlock, at: (u32, u32), spec: &PasteSpec) -> PasteExtras {
    let mut ex = PasteExtras::default();
    let what = spec.what;
    if what == PasteWhat::Comments || what.all() {
        for n in &clip.notes {
            if let Some((r, c)) = clip.dest(at, n.at, spec.transpose) {
                ex.notes.push((r, c, n.author.clone(), n.text.clone()));
            }
        }
    }
    if what == PasteWhat::Validation || what.all() {
        ex.clear_rules = Some(clip.pasted_rect(at, spec.transpose));
        for rule in &clip.rules {
            let cells: Vec<(u32, u32)> = rule
                .cells
                .iter()
                .filter_map(|&ij| clip.dest(at, ij, spec.transpose))
                .collect();
            // The formulas read relative to the source rule's anchor; each
            // rectangle pasted is a rule of its own, anchored at its
            // top-left, so it reads them moved by the distance from the old
            // anchor to that corner (#707 r8 M1).
            for rect in cells_to_rects(&cells) {
                let dr = i64::from(rect.0) - i64::from(rule.anchor.0);
                let dc = i64::from(rect.1) - i64::from(rule.anchor.1);
                let mv = |f: &str| {
                    if f.is_empty() || (dr, dc) == (0, 0) {
                        f.to_string()
                    } else {
                        translate_formula(f, dr, dc).unwrap_or_else(|| f.to_string())
                    }
                };
                let moved = ClipRule {
                    formula1: mv(&rule.formula1),
                    formula2: mv(&rule.formula2),
                    cells: Vec::new(),
                    anchor: (rect.0, rect.1),
                    ..rule.clone()
                };
                ex.rules.push((rect, moved));
            }
        }
    }
    if matches!(
        what,
        PasteWhat::ColumnWidths | PasteWhat::AllAndColumnWidths
    ) && !spec.transpose
    {
        for (j, &w) in clip.widths.iter().enumerate() {
            let c = u64::from(at.1) + j as u64;
            if c < u64::from(MAX_COLS) {
                ex.widths.push((c as u32, w));
            }
        }
    }
    ex
}

/// Take `rect` out of every validation rule on `sheet`: a rule's ranges are
/// cut around it, and a rule left with no range goes (its element named in
/// `dv_removed` for the save).
pub fn clear_validation(sheet: &mut Sheet, rect: Area) {
    let removed = &mut sheet.dv_removed;
    sheet.validations.retain_mut(|dv| {
        if !dv.ranges.iter().any(|&r| overlaps(r, rect)) {
            return true;
        }
        let before = rule_anchor(&dv.ranges);
        dv.ranges = dv.ranges.iter().flat_map(|&r| subtract(r, rect)).collect();
        if dv.ranges.is_empty() {
            removed.extend(dv.ix);
            return false;
        }
        // The formulas read relative to the anchor: one that moved takes
        // them along, so every cell left still reads the cells it read
        // (#707 r9 M1).
        let after = rule_anchor(&dv.ranges);
        let dr = i64::from(after.0) - i64::from(before.0);
        let dc = i64::from(after.1) - i64::from(before.1);
        if (dr, dc) != (0, 0) {
            for f in [&mut dv.formula1, &mut dv.formula2] {
                if !f.is_empty() {
                    if let Some(moved) = translate_formula(f, dr, dc) {
                        *f = moved;
                    }
                }
            }
        }
        true
    });
}

/// A rule's anchor: the top-left of its ranges, which its formulas are
/// relative to (as `shift_rule_ranges` reads it).
pub(crate) fn rule_anchor(ranges: &[Area]) -> (u32, u32) {
    ranges
        .iter()
        .fold((u32::MAX, u32::MAX), |(r, c), &(r1, c1, _, _)| {
            (r.min(r1), c.min(c1))
        })
}

/// `a` without `b`: up to four rectangles (above, below, left, right).
pub(crate) fn subtract(a: Area, b: Area) -> Vec<Area> {
    if !overlaps(a, b) {
        return vec![a];
    }
    let (ar0, ac0, ar1, ac1) = a;
    let (br0, bc0, br1, bc1) = b;
    let mut out = Vec::new();
    if ar0 < br0 {
        out.push((ar0, ac0, br0 - 1, ac1));
    }
    if ar1 > br1 {
        out.push((br1 + 1, ac0, ar1, ac1));
    }
    let (mr0, mr1) = (ar0.max(br0), ar1.min(br1));
    if ac0 < bc0 {
        out.push((mr0, ac0, mr1, bc0 - 1));
    }
    if ac1 > bc1 {
        out.push((mr0, bc1 + 1, mr1, ac1));
    }
    out
}

/// `cells` as rectangles: each row's runs of adjacent columns, then runs
/// that repeat on consecutive rows joined.
pub(crate) fn cells_to_rects(cells: &[(u32, u32)]) -> Vec<Area> {
    let mut sorted = cells.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    // Row runs: (row, c0, c1).
    let mut runs: Vec<(u32, u32, u32)> = Vec::new();
    for (r, c) in sorted {
        match runs.last_mut() {
            Some(run) if run.0 == r && run.2 + 1 == c => run.2 = c,
            _ => runs.push((r, c, c)),
        }
    }
    let mut rects: Vec<Area> = Vec::new();
    // Open rectangles by their column span, extended while the next row has
    // the same run.
    let mut open: std::collections::HashMap<(u32, u32), usize> = std::collections::HashMap::new();
    for (r, c0, c1) in runs {
        match open.get(&(c0, c1)) {
            Some(&k) if rects[k].2 + 1 == r => rects[k].2 = r,
            _ => {
                open.insert((c0, c1), rects.len());
                rects.push((r, c0, r, c1));
            }
        }
    }
    rects.sort_unstable();
    rects
}

/// Paste Link: a formula in each pasted cell reading the copied cell it
/// stands for. One copied cell is linked absolutely (`=$A$1`), a range
/// relatively (`=A1`), each qualified with the copy's sheet when pasted on
/// another sheet. A blank copied cell is linked too (it shows 0). The
/// destination's formats stay.
pub fn paste_link_changes(
    wb: &Workbook,
    sheet: usize,
    at: (u32, u32),
    clip: &ClipBlock,
) -> Vec<(u32, u32, Cell)> {
    let Some(s) = wb.sheets.get(sheet) else {
        return Vec::new();
    };
    let single = clip.rows.len() == 1 && clip.cols.len() == 1;
    let prefix = if sheet == clip.sheet {
        String::new()
    } else {
        format!("{}!", crate::sheet::quote_sheet_name(&clip.sheet_name))
    };
    let mut out = Vec::new();
    for (i, &r) in clip.rows.iter().enumerate() {
        for (j, &c) in clip.cols.iter().enumerate() {
            let Some(dest) = clip.dest(at, (i, j), false) else {
                continue;
            };
            let name = cell_name(r, c);
            let target = if single {
                let split = name.find(|ch: char| ch.is_ascii_digit()).unwrap_or(0);
                format!("${}${}", &name[..split], &name[split..])
            } else {
                name
            };
            let mut cell = Cell::formula(&format!("{prefix}{target}"));
            cell.style = s.cell(dest.0, dest.1).map_or(0, |d| d.style);
            out.push((dest.0, dest.1, cell));
        }
    }
    out
}

/// What [`paste_special`] did beyond the cells it wrote: the rectangle it
/// covered, and the notes and validation rules for a host with a package to
/// write (a workbook alone has nowhere to keep them).
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub struct Pasted {
    pub rect: Area,
    pub notes: Vec<(u32, u32, String, String)>,
    pub rules: Vec<(Area, ClipRule)>,
}

/// Paste Special `clip` at `at` on `sheet` of `wb`: the cell writes, the
/// column widths and the validation cleared from the destination; the
/// notes and rules to add come back for the host. Refused when nothing of
/// the copy would land on the grid.
#[cfg(test)]
pub fn paste_special(
    wb: &mut Workbook,
    sheet: usize,
    at: (u32, u32),
    clip: &ClipBlock,
    spec: &PasteSpec,
) -> Result<Pasted, String> {
    if sheet >= wb.sheets.len() {
        return Err("No such sheet".into());
    }
    if clip.cells.is_empty() || at.0 >= MAX_ROWS || at.1 >= MAX_COLS {
        return Err("Nothing to paste".into());
    }
    let changes = paste_special_changes(wb, sheet, at, clip, spec)?;
    let ex = paste_special_extras(clip, at, spec);
    let s = &mut wb.sheets[sheet];
    for (r, c, cell) in changes {
        s.set_cell(r, c, cell);
    }
    for &(c, w) in &ex.widths {
        s.set_col_width(c, w);
    }
    if let Some(rect) = ex.clear_rules {
        clear_validation(s, rect);
    }
    Ok(Pasted {
        rect: clip.pasted_rect(at, spec.transpose),
        notes: ex.notes,
        rules: ex.rules,
    })
}

#[cfg(test)]
#[path = "paste_special/tests.rs"]
mod tests;
