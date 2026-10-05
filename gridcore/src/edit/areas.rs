//! The areas of a multi-area selection, indexed: which cells, links, notes or
//! references fall in any of them, without testing each one against every
//! area (#707 r6). A selection may hold up to `MAX_AREAS` areas (a Go To
//! Special result), so a scan of the areas per cell would multiply.

use std::collections::BTreeMap;

use super::{Area, rects_overlap as overlaps};

/// Leaves of up to this many rectangles are scanned.
const LEAF: usize = 8;

/// A bounding-box tree over rectangles, split at the median alternately by
/// row and column: a query visits only the branches whose box it meets.
#[derive(Clone, Debug, Default)]
pub struct RectIndex {
    rects: Vec<Area>,
    nodes: Vec<Node>,
}

#[derive(Clone, Debug)]
struct Node {
    bbox: Area,
    lo: usize,
    hi: usize,
    kids: Option<(usize, usize)>,
}

impl RectIndex {
    pub fn new(areas: &[Area]) -> RectIndex {
        let mut ix = RectIndex {
            rects: areas.to_vec(),
            nodes: Vec::new(),
        };
        if !ix.rects.is_empty() {
            let n = ix.rects.len();
            ix.build(0, n, 0);
        }
        ix
    }

    fn build(&mut self, lo: usize, hi: usize, depth: u32) -> usize {
        let part = &mut self.rects[lo..hi];
        let bbox = part.iter().fold(part[0], |b, r| {
            (b.0.min(r.0), b.1.min(r.1), b.2.max(r.2), b.3.max(r.3))
        });
        let id = self.nodes.len();
        self.nodes.push(Node {
            bbox,
            lo,
            hi,
            kids: None,
        });
        if hi - lo > LEAF {
            let mid = (hi - lo) / 2;
            if depth.is_multiple_of(2) {
                part.select_nth_unstable_by_key(mid, |r| u64::from(r.0) + u64::from(r.2));
            } else {
                part.select_nth_unstable_by_key(mid, |r| u64::from(r.1) + u64::from(r.3));
            }
            let a = self.build(lo, lo + mid, depth + 1);
            let b = self.build(lo + mid, hi, depth + 1);
            self.nodes[id].kids = Some((a, b));
        }
        id
    }

    /// Whether some rectangle meeting `q` passes `pred`.
    pub(crate) fn any(&self, q: Area, mut pred: impl FnMut(Area) -> bool) -> bool {
        let mut stack = Vec::new();
        if !self.nodes.is_empty() {
            stack.push(0);
        }
        while let Some(k) = stack.pop() {
            let n = &self.nodes[k];
            if !overlaps(n.bbox, q) {
                continue;
            }
            match n.kids {
                Some((a, b)) => stack.extend([a, b]),
                None => {
                    for &r in &self.rects[n.lo..n.hi] {
                        if overlaps(r, q) && pred(r) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Each rectangle meeting `q`, into `out`.
    pub(crate) fn meeting(&self, q: Area, out: &mut Vec<Area>) {
        self.any(q, |r| {
            out.push(r);
            false
        });
    }

    /// Whether a rectangle meets `q`.
    pub(crate) fn meets(&self, q: Area) -> bool {
        self.any(q, |_| true)
    }

    /// Whether a rectangle holds the cell `(r, c)`.
    pub fn holds(&self, r: u32, c: u32) -> bool {
        self.meets((r, c, r, c))
    }
}

/// The entries of `map` (keyed by cell) inside any of `areas`, each once, in
/// sheet order: a walk of the rows the areas span, each entry tested
/// against the index. It costs the entries in those rows, times a log, where
/// a walk per area would pay for every area's rows again.
pub(crate) fn entries_in<'a, V>(
    map: &'a BTreeMap<(u32, u32), V>,
    areas: &[Area],
    ix: &RectIndex,
) -> Vec<((u32, u32), &'a V)> {
    let mut bands: Vec<(u32, u32)> = areas.iter().map(|a| (a.0, a.2)).collect();
    bands.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (a, b) in bands {
        match merged.last_mut() {
            Some(m) if u64::from(a) <= u64::from(m.1) + 1 => m.1 = m.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    let mut out = Vec::new();
    for (a, b) in merged {
        for (&(r, c), v) in map.range((a, 0)..=(b, u32::MAX)) {
            if ix.holds(r, c) {
                out.push(((r, c), v));
            }
        }
    }
    out
}

/// The stored cells of `sheet` inside any of `areas`, each once, in sheet
/// order (see [`entries_in`]).
pub fn cells_in_areas<'a>(
    sheet: &'a crate::sheet::Sheet,
    areas: &[Area],
) -> Vec<((u32, u32), &'a crate::sheet::Cell)> {
    entries_in(&sheet.cells, areas, &RectIndex::new(areas))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_what_a_scan_finds() {
        let areas: Vec<Area> = (0..500u32)
            .map(|k| {
                (
                    k * 7 % 300,
                    k * 13 % 90,
                    k * 7 % 300 + k % 5,
                    k * 13 % 90 + k % 3,
                )
            })
            .collect();
        let ix = RectIndex::new(&areas);
        for r in 0..310 {
            for c in 0..95 {
                let scan = areas.iter().any(|&a| overlaps(a, (r, c, r, c)));
                assert_eq!(ix.holds(r, c), scan, "{r},{c}");
            }
        }
        let q = (10, 10, 40, 12);
        assert_eq!(ix.meets(q), areas.iter().any(|&a| overlaps(a, q)));
        assert!(!RectIndex::new(&[]).holds(0, 0));
        let mut map = BTreeMap::new();
        for r in 0..310 {
            map.insert((r, 5), ());
        }
        let got: Vec<(u32, u32)> = entries_in(&map, &areas, &ix)
            .into_iter()
            .map(|(rc, _)| rc)
            .collect();
        let want: Vec<(u32, u32)> = map
            .keys()
            .copied()
            .filter(|&(r, c)| areas.iter().any(|&a| overlaps(a, (r, c, r, c))))
            .collect();
        assert_eq!(got, want);
    }
}
