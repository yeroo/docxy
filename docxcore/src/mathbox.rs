//! A tiny 2-D layout for math: a fragment is a stack of text `lines` with a
//! `base` row that sits on the surrounding baseline. Used by both the OMML and
//! the legacy MathType decoders so matrices stack onto real rows with aligned
//! columns and growing brackets.

#[derive(Clone)]
pub(crate) struct MBox {
    pub lines: Vec<String>,
    pub base: usize,
}

impl MBox {
    pub fn line(s: String) -> MBox {
        MBox {
            lines: vec![s],
            base: 0,
        }
    }
    pub fn empty() -> MBox {
        MBox::line(String::new())
    }
    pub fn width(&self) -> usize {
        self.lines
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
    }
    pub fn is_blank(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }
    /// Flatten to one inline string (used when a construct can't stack).
    pub fn flat(&self) -> String {
        self.lines.join("")
    }
}

/// Place boxes side by side, aligning their baselines.
pub(crate) fn hcat(boxes: &[MBox]) -> MBox {
    let boxes: Vec<&MBox> = boxes.iter().filter(|b| !b.is_blank()).collect();
    match boxes.len() {
        0 => return MBox::empty(),
        1 => return boxes[0].clone(),
        _ => {}
    }
    let above = boxes.iter().map(|b| b.base).max().unwrap();
    let below = boxes
        .iter()
        .map(|b| b.lines.len() - 1 - b.base)
        .max()
        .unwrap();
    let h = above + below + 1;
    let mut out = vec![String::new(); h];
    for b in &boxes {
        let bw = b.width();
        let top = above - b.base;
        for (r, row) in out.iter_mut().enumerate() {
            if r >= top && r - top < b.lines.len() {
                let l = &b.lines[r - top];
                row.push_str(l);
                row.push_str(&" ".repeat(bw - l.chars().count()));
            } else {
                row.push_str(&" ".repeat(bw));
            }
        }
    }
    MBox {
        lines: out,
        base: above,
    }
}

/// Stack boxes vertically (top to bottom), left-aligned, with the given baseline
/// row index. Used for piles / stacked limits.
pub(crate) fn vstack(boxes: &[MBox], base: usize) -> MBox {
    let mut lines = Vec::new();
    for b in boxes {
        lines.extend(b.lines.iter().cloned());
    }
    if lines.is_empty() {
        return MBox::empty();
    }
    let base = base.min(lines.len() - 1);
    MBox { lines, base }
}

/// Wrap a box in growing brackets (`l`/`r`), tall when it spans many rows.
pub(crate) fn bracket(b: MBox, l: char, r: char) -> MBox {
    let h = b.lines.len();
    if h <= 1 {
        return MBox::line(format!(
            "{l}{}{r}",
            b.lines.first().cloned().unwrap_or_default()
        ));
    }
    let (lt, lm, lb, rt, rm, rb) = bracket_pieces(l, r);
    let lines = b
        .lines
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (lc, rc) = if i == 0 {
                (lt, rt)
            } else if i == h - 1 {
                (lb, rb)
            } else {
                (lm, rm)
            };
            format!("{lc} {c} {rc}")
        })
        .collect();
    MBox {
        lines,
        base: b.base,
    }
}

/// Top/middle/bottom glyphs for a growing left/right bracket pair.
fn bracket_pieces(l: char, r: char) -> (char, char, char, char, char, char) {
    match (l, r) {
        ('[', ']') => ('⎡', '⎢', '⎣', '⎤', '⎥', '⎦'),
        ('(', ')') => ('⎛', '⎜', '⎝', '⎞', '⎟', '⎠'),
        ('{', '}') => ('⎧', '⎪', '⎩', '⎫', '⎪', '⎭'),
        ('|', '|') => ('│', '│', '│', '│', '│', '│'),
        _ => (l, '│', l, r, '│', r),
    }
}

/// Lay a grid of cells out as a text table (columns aligned), without brackets.
/// Cells are flattened to one line each.
pub(crate) fn grid(rows: &[Vec<MBox>]) -> MBox {
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if rows.is_empty() || ncols == 0 {
        return MBox::empty();
    }
    let cell =
        |r: usize, c: usize| -> String { rows[r].get(c).map(MBox::flat).unwrap_or_default() };
    let colw: Vec<usize> = (0..ncols)
        .map(|c| {
            (0..rows.len())
                .map(|r| cell(r, c).chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let lines: Vec<String> = (0..rows.len())
        .map(|r| {
            (0..ncols)
                .map(|c| {
                    let s = cell(r, c);
                    format!("{s}{}", " ".repeat(colw[c] - s.chars().count()))
                })
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect();
    let n = lines.len();
    MBox {
        base: (n - 1) / 2,
        lines,
    }
}

/// A bracketed grid (used for standalone matrices).
pub(crate) fn matrix_grid(rows: &[Vec<MBox>], lb: char, rb: char) -> MBox {
    if rows.is_empty() || rows.iter().all(|r| r.is_empty()) {
        return MBox::line(format!("{lb}{rb}"));
    }
    bracket(grid(rows), lb, rb)
}

/// Flatten a finished box to a string (rows joined by `\n`, blank edge rows
/// dropped, single-line results trimmed).
pub(crate) fn flatten(b: &MBox) -> String {
    if b.lines.len() == 1 {
        return b.lines[0].trim().to_string();
    }
    let mut rows: Vec<String> = b.lines.iter().map(|l| l.trim_end().to_string()).collect();
    while rows.first().is_some_and(|l| l.is_empty()) {
        rows.remove(0);
    }
    while rows.last().is_some_and(|l| l.is_empty()) {
        rows.pop();
    }
    rows.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(s: &str) -> MBox {
        MBox::line(s.to_string())
    }

    #[test]
    fn matrix_grid_stacks_rows_with_brackets() {
        let rows = vec![vec![cell("a"), cell("b")], vec![cell("c"), cell("d")]];
        assert_eq!(flatten(&matrix_grid(&rows, '[', ']')), "⎡ a  b ⎤\n⎣ c  d ⎦");
    }

    #[test]
    fn hcat_aligns_text_to_a_tall_box_baseline() {
        let m = matrix_grid(&[vec![cell("a")], vec![cell("b")]], '[', ']');
        // "A=" sits on the matrix's baseline (top) row; row two is indented
        assert_eq!(flatten(&hcat(&[cell("A="), m])), "A=⎡ a ⎤\n  ⎣ b ⎦");
    }
}
