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

/// A rectangle (r0, c0, r1, c1), inclusive.
pub type Rect = (u32, u32, u32, u32);

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
}

impl ClipBlock {
    /// The cells of `rows` × `cols` on `sheet`, with their column widths and
    /// the validation over them.
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
                let mut cells = Vec::new();
                for (i, &r) in rows.iter().enumerate() {
                    for (j, &c) in cols.iter().enumerate() {
                        if dv.covers(r, c) {
                            cells.push((i, j));
                        }
                    }
                }
                (!cells.is_empty()).then(|| ClipRule {
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
        }
    }

    /// The notes among `notes` — `(row, col, author, text)` on the copy's
    /// sheet — that sit on copied cells.
    pub fn set_notes(&mut self, notes: impl IntoIterator<Item = (u32, u32, String, String)>) {
        self.notes = notes
            .into_iter()
            .filter_map(|(r, c, author, text)| {
                let i = self.rows.iter().position(|&x| x == r)?;
                let j = self.cols.iter().position(|&x| x == c)?;
                Some(ClipNote {
                    at: (i, j),
                    author,
                    text,
                })
            })
            .collect();
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
    pub fn pasted_rect(&self, at: (u32, u32), transpose: bool) -> Rect {
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
    ) -> Option<String> {
        let dr = i64::from(dest.0) - i64::from(self.rows[i]);
        let dc = i64::from(dest.1) - i64::from(self.cols[j]);
        if transpose {
            let inside = |row: i64, col: i64| -> Option<(i64, i64)> {
                let i = self.rows.iter().position(|&x| i64::from(x) == row)?;
                let j = self.cols.iter().position(|&x| i64::from(x) == col)?;
                Some((i64::from(at.0) + j as i64, i64::from(at.1) + i as i64))
            };
            let t = Transposed {
                src_sheet: &self.sheet_name,
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
pub fn multi_area_shape(areas: &[Rect]) -> Result<(Vec<u32>, Vec<u32>), &'static str> {
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

fn is_blank(cell: &Cell) -> bool {
    cell.value.is_empty() && cell.formula.is_none()
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
) -> Vec<(u32, u32, Cell)> {
    let mut out = Vec::new();
    if !spec.what.pastes_contents() && spec.what != PasteWhat::Formats {
        return out;
    }
    let Workbook { sheets, styles, .. } = wb;
    let Some(s) = sheets.get(sheet) else {
        return out;
    };
    for (i, row) in clip.cells.iter().enumerate() {
        for (j, src) in row.iter().enumerate() {
            let Some(dest) = clip.dest(at, (i, j), spec.transpose) else {
                continue;
            };
            if spec.skip_blanks && is_blank(src) {
                continue;
            }
            let d = s.cell(dest.0, dest.1).cloned().unwrap_or_default();
            let style = match spec.what {
                PasteWhat::All
                | PasteWhat::AllAndColumnWidths
                | PasteWhat::Formats
                | PasteWhat::ValuesAndSourceFormatting => src.style,
                PasteWhat::AllExceptBorders => {
                    let mut xf = styles.xf(src.style);
                    xf.border = false;
                    styles.intern(xf)
                }
                PasteWhat::FormulasAndNumberFormats | PasteWhat::ValuesAndNumberFormats => {
                    let from = styles.xf(src.style);
                    let mut xf = styles.xf(d.style);
                    xf.numfmt = from.numfmt;
                    xf.code = from.code;
                    styles.intern(xf)
                }
                _ => d.style,
            };
            if spec.what == PasteWhat::Formats {
                if style != d.style || s.cell(dest.0, dest.1).is_none() {
                    out.push((dest.0, dest.1, Cell { style, ..d }));
                }
                continue;
            }
            let values_only = matches!(
                spec.what,
                PasteWhat::Values
                    | PasteWhat::ValuesAndNumberFormats
                    | PasteWhat::ValuesAndSourceFormatting
            );
            let formula = if values_only {
                None
            } else {
                src.formula.as_deref().map(|f| {
                    clip.moved_formula(f, (i, j), dest, at, spec.transpose)
                        .unwrap_or_else(|| f.to_string())
                })
            };
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
                    Err(f) => Cell::formula(&f),
                };
                cell.style = style;
                out.push((dest.0, dest.1, cell));
                continue;
            }
            let mut cell = match formula {
                Some(f) => {
                    let mut c = src.clone();
                    c.formula = Some(f);
                    if !spec.what.all() {
                        c.spill = None;
                    }
                    c
                }
                None if values_only || !spec.what.all() => Cell {
                    value: src.value.clone(),
                    ..Cell::default()
                },
                None => src.clone(),
            };
            cell.style = style;
            out.push((dest.0, dest.1, cell));
        }
    }
    out
}

/// The parts of a Paste Special that are not cell writes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PasteExtras {
    /// Notes to write: (row, col, author, text).
    pub notes: Vec<(u32, u32, String, String)>,
    /// Validation to clear from these cells first, then rules to add, one
    /// rectangle each.
    pub clear_rules: Option<Rect>,
    pub rules: Vec<(Rect, ClipRule)>,
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
            let Some(&first) = cells.first() else {
                continue;
            };
            // The rule's relative references move with its first cell.
            let (i0, j0) = rule.cells[0];
            let dr = i64::from(first.0) - i64::from(clip.rows[i0]);
            let dc = i64::from(first.1) - i64::from(clip.cols[j0]);
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
                ..rule.clone()
            };
            for rect in cells_to_rects(&cells) {
                ex.rules.push((rect, moved.clone()));
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
pub fn clear_validation(sheet: &mut Sheet, rect: Rect) {
    let removed = &mut sheet.dv_removed;
    sheet.validations.retain_mut(|dv| {
        if !dv.ranges.iter().any(|&r| overlaps(r, rect)) {
            return true;
        }
        dv.ranges = dv.ranges.iter().flat_map(|&r| subtract(r, rect)).collect();
        if dv.ranges.is_empty() {
            removed.extend(dv.ix);
            return false;
        }
        true
    });
}

pub(crate) fn overlaps(a: Rect, b: Rect) -> bool {
    a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
}

/// `a` without `b`: up to four rectangles (above, below, left, right).
pub(crate) fn subtract(a: Rect, b: Rect) -> Vec<Rect> {
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
pub fn cells_to_rects(cells: &[(u32, u32)]) -> Vec<Rect> {
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
    let mut rects: Vec<Rect> = Vec::new();
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
#[derive(Clone, Debug, PartialEq)]
pub struct Pasted {
    pub rect: Rect,
    pub notes: Vec<(u32, u32, String, String)>,
    pub rules: Vec<(Rect, ClipRule)>,
}

/// Paste Special `clip` at `at` on `sheet` of `wb`: the cell writes, the
/// column widths and the validation cleared from the destination; the
/// notes and rules to add come back for the host. Refused when nothing of
/// the copy would land on the grid.
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
    let changes = paste_special_changes(wb, sheet, at, clip, spec);
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
