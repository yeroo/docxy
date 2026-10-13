//! Word's table pens (#763): Insert > Table > Draw Table and the table
//! Layout tab's Draw group turn the pointer into a pen or an eraser. The
//! mode, the table geometry each frame records, and the reading of a gesture
//! into a docxcore edit are pure, so they are unit-tested without a window.
//!
//! Points are window pixels. A table's geometry is recorded as it is laid
//! out (`table_el`): each row's top and bottom, its cells on the grid, and the
//! grid's boundaries in twips from the table's left edge, which docxcore's
//! [`DrawnLine`] and [`TableEdge`] speak.

use docxcore::editor::{DrawnLine, TableEdge};

/// A press and release closer than this (px) on both axes is a click: the pen
/// draws nothing.
pub const MIN_DRAG: f32 = 4.0;
/// A drawn rectangle narrower or lower than this (px) makes no table.
pub const MIN_RECT: f32 = 8.0;
/// A vertical line this close (px) to a grid boundary is drawn on it; a
/// horizontal line this close to a row boundary is drawn on that boundary.
pub const SNAP: f32 = 4.0;
/// The eraser takes the nearest line within this distance (px) of the press.
pub const ERASE_REACH: f32 = 6.0;

/// The status line while a pen is on.
pub const PEN_STATUS: &str = "Draw Table: drag to draw a table or a line; Esc ends";
pub const ERASER_STATUS: &str = "Eraser: click a line between cells to merge them; Esc ends";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Eraser,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Pen => "pen",
            Tool::Eraser => "eraser",
        }
    }

    pub fn status(self) -> &'static str {
        match self {
            Tool::Pen => PEN_STATUS,
            Tool::Eraser => ERASER_STATUS,
        }
    }
}

/// The drawing mode: which pen, and the tab it belongs to. The app ends it on
/// Esc, a tab switch, close or open, a dialog and any other command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawMode {
    pub tool: Tool,
    pub tab: usize,
}

impl DrawMode {
    /// A click on the button for `tool` on tab `tab`: the same pen again ends
    /// the mode, otherwise that pen is on. Returns whether a mode is on.
    pub fn click(mode: &mut Option<DrawMode>, tool: Tool, tab: usize) -> bool {
        let same = mode.is_some_and(|m| m.tool == tool && m.tab == tab);
        *mode = (!same).then_some(DrawMode { tool, tab });
        mode.is_some()
    }
}

/// A pen gesture in flight: where it was pressed, where the pointer is now,
/// and whether it is in the open header or footer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    pub from: (f32, f32),
    pub to: (f32, f32),
    pub hf: bool,
}

/// What the window draws over the page: while any of it is up, a press goes
/// to it, not to the pen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overlays {
    pub menu: bool,
    pub ribbon_flyout: bool,
    pub tab_popup: bool,
    pub dialog: bool,
    pub backstage: bool,
}

impl Overlays {
    pub fn cover_page(self) -> bool {
        self.menu || self.ribbon_flyout || self.tab_popup || self.dialog || self.backstage
    }
}

/// Why a press did not start a pen gesture.
pub const NO_PEN: &str = "no table pen is on";
pub const COVERED: &str = "a menu, popup or dialog covers the page";
pub const OFF_PAGE: &str = "the press is not on the page";
pub const OUTSIDE_HF: &str = "the press is outside the open header/footer";

/// One table row as laid out: window px, and its cells as
/// `(first grid column, span)`.
#[derive(Clone, Debug, PartialEq)]
pub struct RowGeom {
    pub top: f32,
    pub bottom: f32,
    pub cells: Vec<(usize, usize)>,
}

/// One table as laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct TableGeom {
    /// It belongs to the open header or footer, not the body.
    pub hf: bool,
    /// Its block path in its story.
    pub path: Vec<usize>,
    /// The window x of the grid's left edge.
    pub left: f32,
    /// Each grid boundary, twips from the left edge (`0` first).
    pub grid: Vec<u32>,
    pub rows: Vec<RowGeom>,
    pub zoom: f32,
}

impl TableGeom {
    fn px(&self, tw: u32) -> f32 {
        self.left + crate::tw_px(tw as i32, self.zoom)
    }

    fn tw(&self, x: f32) -> u32 {
        crate::px_tw((x - self.left).max(0.0), self.zoom).max(0) as u32
    }

    fn top(&self) -> f32 {
        self.rows.first().map_or(0.0, |r| r.top)
    }

    fn bottom(&self) -> f32 {
        self.rows.last().map_or(0.0, |r| r.bottom)
    }

    fn right(&self) -> f32 {
        self.px(self.grid.last().copied().unwrap_or(0))
    }

    /// Whether `(x, y)` lies within the table, grown by `by` px on each side.
    pub fn holds(&self, (x, y): (f32, f32), by: f32) -> bool {
        !self.rows.is_empty()
            && x >= self.left - by
            && x < self.right() + by
            && y >= self.top() - by
            && y < self.bottom() + by
    }

    /// The row at window y.
    fn row_at(&self, y: f32) -> Option<usize> {
        self.rows.iter().position(|r| y >= r.top && y < r.bottom)
    }

    /// The grid column at window x.
    fn col_at(&self, x: f32) -> Option<usize> {
        self.grid
            .windows(2)
            .position(|w| x >= self.px(w[0]) && x < self.px(w[1]))
    }
}

/// A top-level paragraph as laid out (window px).
#[derive(Clone, Debug, PartialEq)]
pub struct ParaGeom {
    pub hf: bool,
    pub block: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// What one frame laid out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub tables: Vec<TableGeom>,
    pub paras: Vec<ParaGeom>,
    /// The open header's or footer's area, `(x, y, w, h)`, on its edit page.
    pub hf_regions: Vec<(f32, f32, f32, f32)>,
}

impl Frame {
    /// The story a press at `at` draws in: the body, or with a header or
    /// footer open (`hf_open`), that header or footer when the press is in
    /// its area, and none elsewhere (the body takes no edit then, as it
    /// takes no click).
    pub fn story_at(&self, hf_open: bool, (x, y): (f32, f32)) -> Option<bool> {
        if !hf_open {
            return Some(false);
        }
        self.hf_regions
            .iter()
            .any(|&(rx, ry, rw, rh)| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
            .then_some(true)
    }

    /// The innermost table of the story holding `at` (grown by `by` px).
    fn table_at(&self, hf: bool, at: (f32, f32), by: f32) -> Option<&TableGeom> {
        self.tables
            .iter()
            .filter(|t| t.hf == hf && t.holds(at, by))
            .max_by_key(|t| t.path.len())
    }

    /// The top-level paragraph a new table drawn from `at` goes after: the
    /// one under it, else the last one above it (in its column), else the
    /// first one.
    pub fn para_for(&self, hf: bool, (x, y): (f32, f32)) -> Option<usize> {
        let paras = || self.paras.iter().filter(|p| p.hf == hf);
        let across = |p: &&ParaGeom| x >= p.x && x < p.x + p.w;
        paras()
            .find(|p| across(p) && y >= p.y && y < p.y + p.h)
            .or_else(|| {
                paras()
                    .filter(|p| across(p) && p.y <= y)
                    .max_by(|a, b| a.y.total_cmp(&b.y))
            })
            .or_else(|| {
                paras()
                    .filter(|p| p.y <= y)
                    .max_by(|a, b| a.y.total_cmp(&b.y))
            })
            .or_else(|| paras().min_by_key(|p| p.block))
            .map(|p| p.block)
    }
}

/// The geometry of the frame on screen. The view rotates it as a frame
/// begins (as `Probes` does): `next` fills while that frame is laid out, and
/// input, dispatched between frames, reads it whole.
#[derive(Debug, Default)]
pub struct DrawGeom {
    next: Frame,
    last: Frame,
}

impl DrawGeom {
    /// A frame begins.
    pub fn rotate(&mut self) {
        self.last = std::mem::take(&mut self.next);
    }

    /// The frame on screen: `next`, or the one before when nothing has been
    /// recorded since the rotation.
    pub fn on_screen(&self) -> &Frame {
        if self.next == Frame::default() {
            &self.last
        } else {
            &self.next
        }
    }

    /// A table is being laid out (its rows' positions follow, `row`).
    pub fn table(&mut self, t: TableGeom) {
        self.next
            .tables
            .retain(|o| (o.hf, &o.path) != (t.hf, &t.path));
        self.next.tables.push(t);
    }

    /// Row `ri` of the table at `path` was laid out at `left`, `top..bottom`.
    pub fn row(&mut self, hf: bool, path: &[usize], ri: usize, left: f32, top: f32, bottom: f32) {
        let Some(t) = self
            .next
            .tables
            .iter_mut()
            .find(|t| t.hf == hf && t.path == path)
        else {
            return;
        };
        if ri == 0 {
            t.left = left;
        }
        if let Some(r) = t.rows.get_mut(ri) {
            r.top = top;
            r.bottom = bottom;
        }
    }

    pub fn para(&mut self, p: ParaGeom) {
        self.next.paras.push(p);
    }

    /// The open header's or footer's area was laid out.
    pub fn hf_region(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.next.hf_regions.push((x, y, w, h));
    }
}

/// The box a pen gesture's preview draws, as two corners (window px): the
/// rectangle of a new table, or inside a table, the line along the drag's
/// main axis (a box one pixel thin).
pub fn preview(d: Drag, in_table: bool) -> ((f32, f32), (f32, f32)) {
    let (from, to) = (d.from, d.to);
    if !in_table {
        return (from, to);
    }
    if (to.0 - from.0).abs() >= (to.1 - from.1).abs() {
        let y = (from.1 + to.1) / 2.0;
        ((from.0, y), (to.0, y))
    } else {
        let x = (from.0 + to.0) / 2.0;
        ((x, from.1), (x, to.1))
    }
}

/// What a pen or eraser gesture does.
#[derive(Clone, Debug, PartialEq)]
pub enum Gesture {
    /// A one-cell table of this size (twips), after the paragraph under the
    /// press (or the last one above it): never inside or before it.
    NewTable {
        w_tw: u32,
        h_tw: u32,
    },
    /// A line drawn across the table at `table`.
    Line {
        table: Vec<usize>,
        line: DrawnLine,
    },
    /// A line erased.
    Erase {
        table: Vec<usize>,
        edge: TableEdge,
    },
    /// Refused, with the reason for the status line.
    Refused(&'static str),
    Nothing,
}

const OUTER: &str = "the table's outer border cannot be erased";

/// Read a gesture from `from` (the press) to `to` (the release) on the frame
/// `frame`, in the body or the open header/footer (`hf`), at `zoom`.
pub fn classify(
    frame: &Frame,
    hf: bool,
    tool: Tool,
    from: (f32, f32),
    to: (f32, f32),
    zoom: f32,
) -> Gesture {
    match tool {
        Tool::Pen => pen(frame, hf, from, to, zoom),
        Tool::Eraser => eraser(frame, hf, from),
    }
}

fn pen(frame: &Frame, hf: bool, from: (f32, f32), to: (f32, f32), zoom: f32) -> Gesture {
    let (dx, dy) = ((to.0 - from.0).abs(), (to.1 - from.1).abs());
    if dx < MIN_DRAG && dy < MIN_DRAG {
        return Gesture::Nothing;
    }
    let Some(t) = frame.table_at(hf, from, 0.0) else {
        if dx < MIN_RECT || dy < MIN_RECT {
            return Gesture::Nothing;
        }
        let tw = |p: f32| crate::px_tw(p, zoom.max(f32::EPSILON)).max(0) as u32;
        return Gesture::NewTable {
            w_tw: tw(dx),
            h_tw: tw(dy),
        };
    };
    let table = t.path.clone();
    if dx >= dy {
        let y = (from.1 + to.1) / 2.0;
        // On the table's top or bottom border: there is a line there already.
        if (y - t.top()).abs() <= SNAP || (y - t.bottom()).abs() <= SNAP {
            return Gesture::Nothing;
        }
        let (x0, x1) = (t.tw(from.0.min(to.0)), t.tw(from.0.max(to.0)));
        let under = t.rows[..t.rows.len() - 1]
            .iter()
            .enumerate()
            .map(|(r, row)| (r, (row.bottom - y).abs()))
            .filter(|&(_, d)| d <= SNAP)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((row, _)) = under {
            return Gesture::Line {
                table,
                line: DrawnLine::Under { row, x0, x1 },
            };
        }
        let Some(row) = t.row_at(y) else {
            return Gesture::Nothing;
        };
        let at = crate::px_tw(y - t.rows[row].top, t.zoom).max(0) as u32;
        return Gesture::Line {
            table,
            line: DrawnLine::Across { row, x0, x1, at },
        };
    }
    let x = (from.0 + to.0) / 2.0;
    let x_tw = t
        .grid
        .iter()
        .copied()
        .map(|b| (b, (t.px(b) - x).abs()))
        .filter(|&(_, d)| d <= SNAP)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or_else(|| t.tw(x), |(b, _)| b);
    if x_tw == 0 || x_tw >= t.grid.last().copied().unwrap_or(0) {
        return Gesture::Nothing;
    }
    let (y0, y1) = (from.1.min(to.1), from.1.max(to.1));
    let mut rows: Vec<usize> = t
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            let mid = (r.top + r.bottom) / 2.0;
            mid >= y0 && mid <= y1
        })
        .map(|(i, _)| i)
        .collect();
    rows.extend(t.row_at(from.1));
    let (Some(&top), Some(&bottom)) = (rows.iter().min(), rows.iter().max()) else {
        return Gesture::Nothing;
    };
    Gesture::Line {
        table,
        line: DrawnLine::Vertical {
            x: x_tw,
            top,
            bottom,
        },
    }
}

fn eraser(frame: &Frame, hf: bool, at: (f32, f32)) -> Gesture {
    let Some(t) = frame.table_at(hf, at, ERASE_REACH) else {
        return Gesture::Nothing;
    };
    let (x, y) = at;
    // (distance, what) for each line near the press.
    let mut near: Vec<(f32, Gesture)> = Vec::new();
    let edge = |edge| Gesture::Erase {
        table: t.path.clone(),
        edge,
    };
    if let Some(r) = t.row_at(y.clamp(t.top(), t.bottom() - 0.01)) {
        for &(s, n) in &t.rows[r].cells {
            for col in [s, s + n] {
                if let Some(&b) = t.grid.get(col) {
                    near.push((
                        (t.px(b) - x).abs(),
                        edge(TableEdge::Vertical { row: r, col }),
                    ));
                }
            }
        }
    }
    let col = t.col_at(x.clamp(t.left, t.right() - 0.01));
    if let Some(col) = col {
        for (r, row) in t.rows.iter().enumerate() {
            near.push((
                (row.bottom - y).abs(),
                edge(TableEdge::Horizontal { row: r, col }),
            ));
        }
        near.push(((t.top() - y).abs(), Gesture::Refused(OUTER)));
    }
    near.into_iter()
        .filter(|(d, _)| *d <= ERASE_REACH)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map_or(Gesture::Nothing, |(_, g)| g)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×2 body table at path [1]: columns of 1500 twips (100 px at zoom
    /// 1), rows 30 px high from y = 100, left edge at x = 50.
    fn two_by_two() -> TableGeom {
        TableGeom {
            hf: false,
            path: vec![1],
            left: 50.0,
            grid: vec![0, 1500, 3000],
            rows: vec![
                RowGeom {
                    top: 100.0,
                    bottom: 130.0,
                    cells: vec![(0, 1), (1, 1)],
                },
                RowGeom {
                    top: 130.0,
                    bottom: 160.0,
                    cells: vec![(0, 1), (1, 1)],
                },
            ],
            zoom: 1.0,
        }
    }

    fn frame(tables: Vec<TableGeom>) -> Frame {
        Frame {
            tables,
            hf_regions: Vec::new(),
            paras: vec![
                ParaGeom {
                    hf: false,
                    block: 0,
                    x: 50.0,
                    y: 60.0,
                    w: 600.0,
                    h: 20.0,
                },
                ParaGeom {
                    hf: false,
                    block: 2,
                    x: 50.0,
                    y: 180.0,
                    w: 600.0,
                    h: 20.0,
                },
            ],
        }
    }

    fn pen_on(f: &Frame, from: (f32, f32), to: (f32, f32)) -> Gesture {
        let zoom = f.tables.first().map_or(1.0, |t| t.zoom);
        classify(f, false, Tool::Pen, from, to, zoom)
    }

    #[test]
    fn the_buttons_toggle_and_switch_pens() {
        let mut m = None;
        assert!(DrawMode::click(&mut m, Tool::Pen, 0));
        assert_eq!(m.map(|m| m.tool), Some(Tool::Pen));
        // The other pen switches; the same one again ends the mode.
        assert!(DrawMode::click(&mut m, Tool::Eraser, 0));
        assert_eq!(m.map(|m| m.tool), Some(Tool::Eraser));
        assert!(!DrawMode::click(&mut m, Tool::Eraser, 0));
        assert!(m.is_none());
        // A mode left on another tab is replaced, not ended.
        let mut m = Some(DrawMode {
            tool: Tool::Pen,
            tab: 1,
        });
        assert!(DrawMode::click(&mut m, Tool::Pen, 0));
        assert_eq!(m.map(|m| m.tab), Some(0));
    }

    #[test]
    fn every_overlay_covers_the_page() {
        assert!(!Overlays::default().cover_page());
        for o in [
            Overlays {
                menu: true,
                ..Default::default()
            },
            Overlays {
                ribbon_flyout: true,
                ..Default::default()
            },
            Overlays {
                tab_popup: true,
                ..Default::default()
            },
            Overlays {
                dialog: true,
                ..Default::default()
            },
            Overlays {
                backstage: true,
                ..Default::default()
            },
        ] {
            assert!(o.cover_page(), "{o:?}");
        }
    }

    #[test]
    fn the_preview_is_the_rectangle_or_the_line_drawn() {
        let d = Drag {
            from: (10.0, 10.0),
            to: (50.0, 20.0),
            hf: false,
        };
        assert_eq!(preview(d, false), ((10.0, 10.0), (50.0, 20.0)));
        assert_eq!(preview(d, true), ((10.0, 15.0), (50.0, 15.0)));
        let d = Drag {
            to: (14.0, 60.0),
            ..d
        };
        assert_eq!(preview(d, true), ((12.0, 10.0), (12.0, 60.0)));
    }

    #[test]
    fn a_drag_off_every_table_draws_a_new_one() {
        let f = frame(vec![two_by_two()]);
        assert_eq!(
            pen_on(&f, (100.0, 300.0), (250.0, 360.0)),
            Gesture::NewTable {
                w_tw: 2250,
                h_tw: 900
            }
        );
        // Drawn up and to the left: the same rectangle.
        assert_eq!(
            pen_on(&f, (250.0, 360.0), (100.0, 300.0)),
            Gesture::NewTable {
                w_tw: 2250,
                h_tw: 900
            }
        );
    }

    #[test]
    fn a_click_or_tiny_drag_draws_nothing() {
        let f = frame(vec![two_by_two()]);
        assert_eq!(pen_on(&f, (100.0, 300.0), (101.0, 302.0)), Gesture::Nothing);
        assert_eq!(pen_on(&f, (100.0, 300.0), (200.0, 305.0)), Gesture::Nothing);
        assert_eq!(pen_on(&f, (80.0, 110.0), (82.0, 111.0)), Gesture::Nothing);
    }

    #[test]
    fn the_new_table_size_follows_the_zoom() {
        let mut t = two_by_two();
        t.zoom = 2.0;
        let f = frame(vec![t]);
        assert_eq!(
            pen_on(&f, (100.0, 300.0), (200.0, 400.0)),
            Gesture::NewTable {
                w_tw: 750,
                h_tw: 750
            }
        );
    }

    #[test]
    fn a_vertical_drag_draws_a_vertical_line_over_the_rows_it_covers() {
        let f = frame(vec![two_by_two()]);
        // Through row 0's middle only, at x 80 (450 twips).
        assert_eq!(
            pen_on(&f, (80.0, 102.0), (81.0, 128.0)),
            Gesture::Line {
                table: vec![1],
                line: DrawnLine::Vertical {
                    x: 458,
                    top: 0,
                    bottom: 0
                }
            }
        );
        // Over both rows.
        assert_eq!(
            pen_on(&f, (80.0, 102.0), (80.0, 158.0)),
            Gesture::Line {
                table: vec![1],
                line: DrawnLine::Vertical {
                    x: 450,
                    top: 0,
                    bottom: 1
                }
            }
        );
        // Near a grid boundary: snapped onto it.
        assert_eq!(
            pen_on(&f, (148.0, 102.0), (149.0, 158.0)),
            Gesture::Line {
                table: vec![1],
                line: DrawnLine::Vertical {
                    x: 1500,
                    top: 0,
                    bottom: 1
                }
            }
        );
        // Onto the outer border: nothing.
        assert_eq!(pen_on(&f, (52.0, 102.0), (52.0, 158.0)), Gesture::Nothing);
        assert_eq!(pen_on(&f, (247.0, 102.0), (247.0, 158.0)), Gesture::Nothing);
    }

    #[test]
    fn a_horizontal_drag_is_across_a_row_or_under_it() {
        let f = frame(vec![two_by_two()]);
        // Inside row 0, 12 px below its top.
        assert_eq!(
            pen_on(&f, (55.0, 112.0), (140.0, 112.0)),
            Gesture::Line {
                table: vec![1],
                line: DrawnLine::Across {
                    row: 0,
                    x0: 75,
                    x1: 1350,
                    at: 180
                }
            }
        );
        // Within the snap of the boundary under row 0 (hidden in a merge
        // or not): on it.
        assert_eq!(
            pen_on(&f, (55.0, 128.0), (140.0, 128.0)),
            Gesture::Line {
                table: vec![1],
                line: DrawnLine::Under {
                    row: 0,
                    x0: 75,
                    x1: 1350
                }
            }
        );
    }

    #[test]
    fn a_horizontal_drag_on_the_top_or_bottom_border_draws_nothing() {
        let f = frame(vec![two_by_two()]);
        // The top border is at y 100, the bottom at 160; within the snap of
        // either is on it.
        for y in [100.0, 103.0, 157.0, 160.0] {
            assert_eq!(pen_on(&f, (55.0, y), (140.0, y)), Gesture::Nothing, "{y}");
        }
        // Just past the snap, inside the first or last row: across it.
        assert!(matches!(
            pen_on(&f, (55.0, 105.0), (140.0, 105.0)),
            Gesture::Line {
                line: DrawnLine::Across { row: 0, .. },
                ..
            }
        ));
        assert!(matches!(
            pen_on(&f, (55.0, 155.0), (140.0, 155.0)),
            Gesture::Line {
                line: DrawnLine::Across { row: 1, .. },
                ..
            }
        ));
    }

    #[test]
    fn the_innermost_table_takes_the_gesture() {
        let outer = two_by_two();
        let inner = TableGeom {
            path: vec![1, 0, 1, 0],
            left: 160.0,
            grid: vec![0, 600, 1200],
            rows: vec![RowGeom {
                top: 105.0,
                bottom: 125.0,
                cells: vec![(0, 1), (1, 1)],
            }],
            ..two_by_two()
        };
        let f = frame(vec![outer, inner]);
        let g = pen_on(&f, (190.0, 106.0), (190.0, 124.0));
        assert!(
            matches!(&g, Gesture::Line { table, .. } if table == &vec![1, 0, 1, 0]),
            "{g:?}"
        );
        // A header/footer table is not the body's.
        let mut hf = two_by_two();
        hf.hf = true;
        let f = frame(vec![hf]);
        assert!(matches!(
            pen_on(&f, (80.0, 102.0), (120.0, 158.0)),
            Gesture::NewTable { .. }
        ));
    }

    #[test]
    fn the_eraser_takes_the_nearest_line() {
        let f = frame(vec![two_by_two()]);
        let er = |at| classify(&f, false, Tool::Eraser, at, at, 1.0);
        assert_eq!(
            er((147.0, 115.0)),
            Gesture::Erase {
                table: vec![1],
                edge: TableEdge::Vertical { row: 0, col: 1 }
            }
        );
        assert_eq!(
            er((100.0, 132.0)),
            Gesture::Erase {
                table: vec![1],
                edge: TableEdge::Horizontal { row: 0, col: 0 }
            }
        );
        // The outer border is offered to docxcore (it refuses with its
        // reason), or refused here for the top edge.
        assert_eq!(
            er((52.0, 145.0)),
            Gesture::Erase {
                table: vec![1],
                edge: TableEdge::Vertical { row: 1, col: 0 }
            }
        );
        assert_eq!(er((100.0, 98.0)), Gesture::Refused(OUTER));
        // In a cell's middle, far from every line.
        assert_eq!(er((100.0, 115.0)), Gesture::Nothing);
        assert_eq!(er((400.0, 400.0)), Gesture::Nothing);
    }

    #[test]
    fn with_a_header_open_only_its_area_takes_the_pen() {
        let mut f = frame(vec![two_by_two()]);
        f.hf_regions.push((50.0, 10.0, 600.0, 40.0));
        f.paras.push(ParaGeom {
            hf: true,
            block: 0,
            x: 50.0,
            y: 20.0,
            w: 600.0,
            h: 20.0,
        });
        // No header open: the body, wherever.
        assert_eq!(f.story_at(false, (100.0, 30.0)), Some(false));
        // Open: its area is the header's; the body takes nothing.
        assert_eq!(f.story_at(true, (100.0, 30.0)), Some(true));
        assert_eq!(f.story_at(true, (100.0, 300.0)), None);
        assert_eq!(f.story_at(true, (80.0, 110.0)), None, "a body table");
        // In the header, a new table goes after the header's paragraph.
        assert_eq!(f.para_for(true, (100.0, 45.0)), Some(0));
        assert!(matches!(
            classify(&f, true, Tool::Pen, (80.0, 12.0), (200.0, 48.0), 1.0),
            Gesture::NewTable { .. }
        ));
    }

    #[test]
    fn a_new_table_goes_after_the_paragraph_under_or_above_the_press() {
        let f = frame(vec![two_by_two()]);
        assert_eq!(f.para_for(false, (100.0, 65.0)), Some(0));
        assert_eq!(f.para_for(false, (100.0, 170.0)), Some(0), "above it");
        assert_eq!(f.para_for(false, (100.0, 500.0)), Some(2));
        assert_eq!(f.para_for(false, (100.0, 10.0)), Some(0), "the first");
        assert_eq!(f.para_for(true, (100.0, 65.0)), None);
    }

    #[test]
    fn the_geometry_reads_the_frame_on_screen() {
        let mut g = DrawGeom::default();
        let mut t = two_by_two();
        t.left = 0.0;
        g.table(t.clone());
        g.row(false, &[1], 0, 50.0, 100.0, 130.0);
        assert_eq!(g.on_screen().tables[0].left, 50.0);
        g.rotate();
        // Nothing recorded yet this frame: the last one is on screen.
        assert_eq!(g.on_screen().tables.len(), 1);
        g.para(ParaGeom {
            hf: false,
            block: 0,
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        });
        assert!(
            g.on_screen().tables.is_empty(),
            "the new frame has no table"
        );
    }
}
