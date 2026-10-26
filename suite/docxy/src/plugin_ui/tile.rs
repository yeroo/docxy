//! A swatch gallery's palette tile and sections (#1761): the miniature chart
//! Project draws for a Gantt Chart Style tile, and the rows a sectioned
//! gallery lays out. Pure geometry, drawn by `Docxy::swatch_gallery`.

use projcore::uiplugin::Palette;

/// A section header row's height, px.
pub(crate) const HEADER_H: f32 = 16.;

/// The box a palette tile's miniature fills: the tile inside its border and
/// padding.
pub(crate) fn tile_body() -> (f32, f32) {
    let g = crate::style_gallery::TILE;
    let inset = 2. * (g.pad + g.border);
    (g.w - inset, g.h - inset)
}

/// What a mark is drawn as.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MarkShape {
    Rect,
    Diamond,
}

/// One mark of the miniature, in px from the tile body's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Mark {
    pub shape: MarkShape,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: u32,
}

/// The link arrow's colour: neutral, as it belongs to no bar kind.
pub(crate) const LINK_COLOR: u32 = 0x808080;

/// The miniature of `palette` in a `w` x `h` box: a summary bracket on top,
/// the manual bar, the on-track bar below it, a link running down from that
/// bar's end to a milestone diamond. Proportions are by estimate (no capture
/// of Project's tile is in this repository).
pub(crate) fn palette_tile_marks(palette: &Palette, w: f32, h: f32) -> Vec<Mark> {
    use crate::project::tile_color;
    let rect = |x: f32, y: f32, w: f32, h: f32, color: u32| Mark {
        shape: MarkShape::Rect,
        x,
        y,
        w,
        h,
        color,
    };
    let unit = (h / 5.).floor().max(4.);
    let bar_h = (unit * 0.6).floor().max(2.);
    let summary = tile_color(palette, "summary");
    let left = (w * 0.06).floor();
    let right = w - left;
    let post = 2.;
    let bracket_h = (bar_h * 0.5).floor().max(2.);
    let manual_y = unit;
    let track_y = 2. * unit;
    let dia = (unit * 0.9).floor().max(4.);
    let end = (right - dia).max(left + 4.);
    let dia_y = (h - dia).max(track_y + bar_h);
    let link_x = end + (dia / 2.).floor();
    vec![
        rect(left, 0., right - left, bracket_h, summary),
        rect(left, 0., post, bracket_h + bar_h, summary),
        rect(right - post, 0., post, bracket_h + bar_h, summary),
        rect(
            left + 2.,
            manual_y,
            (w * 0.55).floor(),
            bar_h,
            tile_color(palette, "manual"),
        ),
        rect(
            left + (w * 0.2).floor(),
            track_y,
            (w * 0.55).floor() - 2.,
            bar_h,
            tile_color(palette, "on-track"),
        ),
        rect(
            link_x,
            track_y + bar_h,
            1.,
            (dia_y - track_y - bar_h).max(1.),
            LINK_COLOR,
        ),
        Mark {
            shape: MarkShape::Diamond,
            x: end,
            y: dia_y,
            w: dia,
            h: dia,
            color: tile_color(palette, "milestone"),
        },
    ]
}

/// One row of a gallery as laid out.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
    /// A section's header text.
    Header(String),
    /// The tiles of items `start..end`.
    Tiles(usize, usize),
}

/// The rows of a gallery whose items sit in `sections` (empty = none), at
/// most `per_row` tiles to a row: a header before each run of equal
/// non-empty sections, and a new row at every section start. With no
/// section at all this is the plain wrap.
pub(crate) fn gallery_layout(sections: &[&str], per_row: usize) -> Vec<Row> {
    let per_row = per_row.max(1);
    let mut rows = Vec::new();
    let mut i = 0;
    while i < sections.len() {
        let sec = sections[i];
        let mut j = i;
        while j < sections.len() && sections[j] == sec {
            j += 1;
        }
        if !sec.is_empty() {
            rows.push(Row::Header(sec.to_string()));
        }
        let mut k = i;
        while k < j {
            let e = (k + per_row).min(j);
            rows.push(Row::Tiles(k, e));
            k = e;
        }
        i = j;
    }
    if rows.is_empty() {
        rows.push(Row::Tiles(0, 0));
    }
    rows
}

/// The longest run of equal sections: the tiles one row needs room for. With
/// no sections the whole gallery is one run, so this is the item count.
pub(crate) fn span_of(sections: &[&str]) -> usize {
    let mut best = 0;
    let mut run = 0;
    for (i, s) in sections.iter().enumerate() {
        run = if i > 0 && sections[i - 1] == *s {
            run + 1
        } else {
            1
        };
        best = best.max(run);
    }
    best
}

/// The height `rows` draw: tiles `tile_h` tall, headers [`HEADER_H`], `gap`
/// between rows.
pub(crate) fn layout_height(rows: &[Row], tile_h: f32, gap: f32) -> f32 {
    let sum: f32 = rows
        .iter()
        .map(|r| match r {
            Row::Header(_) => HEADER_H,
            Row::Tiles(..) => tile_h,
        })
        .sum();
    sum + gap * (rows.len() as f32 - 1.).max(0.)
}
