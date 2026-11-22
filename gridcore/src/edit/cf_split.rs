//! A cut splits a conditional-format rule whose cells read it differently
//! (#1458).
//!
//! A rule's formulas are written once, for its anchor, and each cell reads
//! them translated to itself. A cut that moves only some of the cells its
//! references read leaves the rule's cells reading the cut differently: one
//! rewrite at the anchor is right for the cells that read it as the anchor
//! does and wrong for the rest. So before the formula walk re-points the
//! rules, each such rule is split into blocks in each of which every cell
//! reads the cut alike: the walk's one rewrite per block is then right for
//! every cell. A split only re-anchors the formulas, so each cell still
//! reads what it read.

use std::collections::HashMap;

use super::{Area, rule_anchor};
use crate::formula::{CellMove, CellRef, Expr, names_own_cells, parse, to_string, translate};
use crate::sheet::{CfKind, CondFormat, Workbook};
use crate::validation::{cut_lines, split_at};

/// What [`split_cond_formats`] did.
#[derive(Default)]
pub(super) struct Split {
    /// Whether it split a block (with `write` off: whether it would).
    pub any: bool,
    /// The blocks split off a rule over the cut that lie beside it, by
    /// (sheet, index in [`crate::sheet::Sheet::cond_formats`]), with the
    /// rule's ranges before the split: the walk re-points them as any rule
    /// beside the cut, but keeps their fully absolute references to the
    /// rule's own cells ([`crate::formula::move_ref_expr_keeping_own`]).
    pub beside_over: HashMap<(usize, usize), Vec<Area>>,
}

/// Split every conditional-format block the cut `mv` off sheet `src` makes
/// its cells read differently, so that the formula walk after it re-points
/// each block whole. Two kinds of block are split:
///
/// - one `take` takes (given its sheet and ranges): its parts that read a
///   cut cell through a reference [`crate::formula::move_ref_expr`] moves
///   (a range: both corners) differently are split apart;
/// - with `over`, one on `src` whose ranges meet the cut (which the hosts
///   leave in place, its cut cells keeping their formulas but for the
///   absolute references, #1687): its part inside the cut stays one block,
///   and when a cell beside the cut reads into it, the cells beside it are
///   split as above, keeping their absolute references to the rule's own
///   cells (#880).
///
/// `take` must take every part of a block it takes, as the walk asks it of
/// each part again. Only blocks judged cell by cell are split (not icon
/// sets, color scales, data bars, top/bottom, average or duplicate rules,
/// which are judged over their whole range, nor a block mixing them).
/// Each split block keeps its rules' formats and priorities; the part
/// holding its first range's top-left cell keeps the block's `ix`, and the
/// others are new blocks right after it whose `copy_of` names the element
/// a save clones for them. With `write` off nothing is written and the
/// answer says only whether a block would be split.
pub(super) fn split_cond_formats(
    wb: &mut Workbook,
    src: usize,
    mv: &CellMove,
    take: impl Fn(usize, &[Area]) -> bool,
    over: bool,
    write: bool,
) -> Split {
    let mut out = Split::default();
    // A cut pasted where it was moves nothing.
    if mv.src.eq_ignore_ascii_case(mv.dst) && (mv.dr, mv.dc) == (0, 0) {
        return out;
    }
    let names: Vec<String> = wb.sheets.iter().map(|s| s.name.clone()).collect();
    let (r0, c0, r1, c1) = mv.rect;
    let meets = |a: &Area| a.0 <= r1 && r0 <= a.2 && a.1 <= c1 && c0 <= a.3;
    for (h, sheet) in wb.sheets.iter_mut().enumerate() {
        let mut blocks = Vec::with_capacity(sheet.cond_formats.len());
        for cf in std::mem::take(&mut sheet.cond_formats) {
            let is_over = over && h == src && cf.ranges.iter().any(meets);
            let groups = if is_over || take(h, &cf.ranges) {
                plan(&cf, &names[h], mv, is_over)
            } else {
                None
            };
            let Some(groups) = groups else {
                blocks.push(cf);
                continue;
            };
            out.any = true;
            if !write {
                // Nothing is written: put the blocks back as they were.
                blocks.push(cf);
                continue;
            }
            let before = cf.ranges.clone();
            let a = rule_anchor(&cf.ranges);
            for (j, g) in groups.into_iter().enumerate() {
                if is_over && !g.inside {
                    out.beside_over.insert((h, blocks.len()), before.clone());
                }
                blocks.push(re_anchored(&cf, a, g.ranges, j == 0));
            }
        }
        sheet.cond_formats = blocks;
        if out.any && !write {
            return out;
        }
    }
    out
}

/// One block a split makes: its ranges, and whether they are the cut cells
/// of a rule over the cut.
struct Group {
    ranges: Vec<Area>,
    inside: bool,
}

/// A reference of a rule's formula that can name a cut cell, as it reads
/// from the rule's anchor: a cell's corner, or a range's two corners (both
/// on the cut's sheet: a range reaching off it never follows the cut).
type Reference = (CellRef, Option<CellRef>);

/// The blocks `cf` (on the sheet named `home`) is split into for the cut
/// `mv`, the first holding its first range's top-left cell; `None` when it
/// isn't split. `over`: its ranges meet the cut on the cut's sheet.
fn plan(cf: &CondFormat, home: &str, mv: &CellMove, over: bool) -> Option<Vec<Group>> {
    let judged_per_cell = cf.rules.iter().all(|r| match &r.kind {
        CfKind::CellIs { .. } | CfKind::Expression { .. } => true,
        CfKind::Other { rule_type, .. } => matches!(
            rule_type.as_str(),
            "containsText"
                | "notContainsText"
                | "beginsWith"
                | "endsWith"
                | "containsBlanks"
                | "notContainsBlanks"
                | "containsErrors"
                | "notContainsErrors"
                | "timePeriod"
        ),
        CfKind::IconSet { .. } => false,
    });
    if !judged_per_cell || cf.rules.iter().any(|r| !r.cfvos().is_empty()) {
        return None;
    }
    let a = rule_anchor(&cf.ranges);
    if a.0 == u32::MAX {
        return None;
    }
    let mut refs = Vec::new();
    for rule in &cf.rules {
        for f in rule.formulas() {
            // A formula that doesn't parse can't be re-anchored.
            let e = parse(f).ok()?;
            references(&e, home, mv.src, &mut refs);
        }
    }
    if refs.is_empty() {
        return None;
    }
    let c = mv.rect;
    let corners: Vec<CellRef> = refs
        .iter()
        .flat_map(|(p, q)| std::iter::once(p.clone()).chain(q.clone()))
        .collect();
    let (mut rows, mut cols) = (Vec::new(), Vec::new());
    cut_lines(&corners, a, c, &mut rows, &mut cols);
    if over {
        rows.extend([i64::from(c.0), i64::from(c.2) + 1]);
        cols.extend([i64::from(c.1), i64::from(c.3) + 1]);
    }
    rows.sort_unstable();
    rows.dedup();
    cols.sort_unstable();
    cols.dedup();
    let inside = |p: &Area| c.0 <= p.0 && p.2 <= c.2 && c.1 <= p.1 && p.3 <= c.3;
    // Which references the walk moves for a cell at `at`.
    let own = if over { cf.ranges.as_slice() } else { &[] };
    let moved = |at: (u32, u32)| moved_refs(&refs, a, at, c, own);
    // Each part of the rule with what decides its rewrite, its groups kept
    // in the order they first appear.
    let mut groups: Vec<(Option<Vec<bool>>, Vec<Area>)> = Vec::new();
    for &range in &cf.ranges {
        for p in split_at(range, &rows, &cols) {
            let key = (!(over && inside(&p))).then(|| moved((p.0, p.1)));
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, parts)) => parts.push(p),
                None => groups.push((key, vec![p])),
            }
        }
    }
    let split = if over {
        // Left whole as #1687 leaves it unless a cell beside the cut reads
        // into it through a relative or mixed reference: a fully absolute
        // one follows the cut (or stays, #880) in the cut cells too.
        let fixed: Vec<bool> = refs
            .iter()
            .map(|(p, q)| {
                [Some(p), q.as_ref()]
                    .into_iter()
                    .flatten()
                    .all(|r| r.abs_row && r.abs_col)
            })
            .collect();
        groups.iter().any(|(k, _)| {
            k.as_ref()
                .is_some_and(|b| b.iter().zip(&fixed).any(|(&b, &fixed)| b && !fixed))
        })
    } else {
        groups.len() > 1
    };
    if !split {
        return None;
    }
    let mut out = Vec::new();
    for (key, parts) in groups {
        let Some(bits) = key else {
            out.push(Group {
                ranges: coalesce(parts),
                inside: true,
            });
            continue;
        };
        // A block's formulas are rewritten at its anchor, the top-left of
        // its ranges, which may be no cell of it (its row from one part, its
        // column from another): when that corner reads the cut otherwise,
        // the parts are packed into blocks whose corners each read it as
        // their parts do. A part alone reads it as itself, so this ends.
        if moved(rule_anchor(&parts)) == bits {
            out.push(Group {
                ranges: coalesce(parts),
                inside: false,
            });
            continue;
        }
        let mut packs: Vec<(Vec<Area>, (u32, u32))> = Vec::new();
        for p in parts {
            let corner = |at: (u32, u32)| (at.0.min(p.0), at.1.min(p.1));
            match packs.iter_mut().find(|(_, at)| moved(corner(*at)) == bits) {
                Some((ps, at)) => {
                    *at = corner(*at);
                    ps.push(p);
                }
                None => packs.push((vec![p], (p.0, p.1))),
            }
        }
        out.extend(packs.into_iter().map(|(ps, _)| Group {
            ranges: coalesce(ps),
            inside: false,
        }));
    }
    Some(out)
}

/// The references of `e` (a rule's formula on sheet `home`) that can name a
/// cell of sheet `src`, as [`crate::formula::move_ref_expr`] reads them: a
/// range's second corner reads its first's sheet.
fn references(e: &Expr, home: &str, src: &str, out: &mut Vec<Reference>) {
    let on = |r: &CellRef, sheet: &str| r.row >= 0 && r.col >= 0 && sheet.eq_ignore_ascii_case(src);
    match e {
        Expr::Ref(r) | Expr::SpillRef(r) => {
            if on(r, r.sheet.as_deref().unwrap_or(home)) {
                out.push((r.clone(), None));
            }
        }
        Expr::Range(p, q) => {
            let first = p.sheet.as_deref().unwrap_or(home);
            if on(p, first) && on(q, q.sheet.as_deref().unwrap_or(first)) {
                out.push((p.clone(), Some(q.clone())));
            }
        }
        Expr::ArrayLit(rows) => rows
            .iter()
            .flatten()
            .for_each(|x| references(x, home, src, out)),
        Expr::Func(_, args) => args.iter().for_each(|x| references(x, home, src, out)),
        Expr::Call(callee, args) => {
            references(callee, home, src, out);
            args.iter().for_each(|x| references(x, home, src, out));
        }
        Expr::Un(_, x) => references(x, home, src, out),
        Expr::Bin(_, l, r) => {
            references(l, home, src, out);
            references(r, home, src, out);
        }
        _ => {}
    }
}

/// For each of `refs` (read from the anchor `a`), whether the cell at `at`
/// reads it naming cells of the cut `c` (a range: both corners), and so
/// follows them; one into the rule's own cells `own` that is fully
/// absolute stays (#880).
fn moved_refs(
    refs: &[Reference],
    a: (u32, u32),
    at: (u32, u32),
    c: Area,
    own: &[Area],
) -> Vec<bool> {
    let (dr, dc) = (
        i64::from(at.0) - i64::from(a.0),
        i64::from(at.1) - i64::from(a.1),
    );
    let cut = |r: &CellRef| {
        let row = if r.abs_row { r.row } else { r.row + dr };
        let col = if r.abs_col { r.col } else { r.col + dc };
        (i64::from(c.0)..=i64::from(c.2)).contains(&row)
            && (i64::from(c.1)..=i64::from(c.3)).contains(&col)
    };
    refs.iter()
        .map(|(p, q)| {
            let q = q.as_ref().unwrap_or(p);
            cut(p) && cut(q) && !names_own_cells(p, q, own)
        })
        .collect()
}

/// `parts` joined where two of them make one rectangle (down, then across,
/// until none do), in reading order of their top-left cells.
fn coalesce(mut parts: Vec<Area>) -> Vec<Area> {
    loop {
        let n = parts.len();
        for down in [true, false] {
            // Rectangles that can join line up on their columns (down) or
            // rows (across), and are sorted along the other way.
            parts.sort_unstable_by_key(|p| {
                if down {
                    (p.1, p.3, p.0)
                } else {
                    (p.0, p.2, p.1)
                }
            });
            let mut joined: Vec<Area> = Vec::with_capacity(parts.len());
            for p in parts.drain(..) {
                match joined.last_mut() {
                    Some(l) if down && (l.1, l.3) == (p.1, p.3) && l.2 + 1 == p.0 => l.2 = p.2,
                    Some(l) if !down && (l.0, l.2) == (p.0, p.2) && l.3 + 1 == p.1 => l.3 = p.3,
                    _ => joined.push(p),
                }
            }
            parts = joined;
        }
        if parts.len() == n {
            break;
        }
    }
    parts.sort_unstable_by_key(|p| (p.0, p.1));
    parts
}

/// `cf` over `ranges` alone, its formulas re-anchored from `a` to their
/// top-left so each cell reads what it read; `first` keeps `cf`'s element
/// claim, any other is a new block cloned from the same element on save.
fn re_anchored(cf: &CondFormat, a: (u32, u32), ranges: Vec<Area>, first: bool) -> CondFormat {
    let to = rule_anchor(&ranges);
    let (dr, dc) = (
        i64::from(to.0) - i64::from(a.0),
        i64::from(to.1) - i64::from(a.1),
    );
    let mut out = CondFormat {
        ranges,
        rules: cf.rules.clone(),
        ix: if first { cf.ix } else { None },
        copy_of: if first {
            cf.copy_of
        } else {
            cf.ix.or(cf.copy_of)
        },
    };
    if (dr, dc) != (0, 0) {
        for rule in &mut out.rules {
            for f in rule.formulas_mut() {
                // `plan` parsed every formula.
                if let Ok(e) = parse(f) {
                    *f = to_string(&translate(&e, dr, dc));
                }
            }
        }
    }
    out
}
