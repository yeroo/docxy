//! The chart's bar style rows (#1696): Project's Bar Styles table and Format
//! Bar override, drawn from the plan's formatting
//! ([`projcore::formatting::bars`]). [`bar_rows`] reads a task's rows against
//! the plan (categories, From and To dates, text fields); [`bar_parts`] turns
//! them into pixel geometry, a pure function so the layout is unit-tested;
//! [`styled_children`] paints the parts inside a [`super::gantt_strip`].
//!
//! A band is [`ROW_H`] tall and a row's `Row` number picks its band, so a
//! view's task rows are as many bands tall as its highest row
//! ([`ProjectView::row_h`]). Within a band the bar zone is [`BAR_Y`] to
//! `BAR_Y + BAR_H`: a full bar fills it, a medium bar is [`MEDIUM_H`] and a
//! narrow one [`NARROW_H`] px tall at its top, middle or bottom; marks are
//! [`MARK`] px squares centred on the zone; the top and bottom texts sit in
//! the bands above and below it and the left, right and inside ones on it.
use super::*;
use projcore::editor::{BarDateKind, Field};
use projcore::formatting::{BarRow, BarText, Mark, Middle, TextStyle as PlanStyle};

/// Top of the bar zone within a band.
pub(crate) const BAR_Y: f32 = 9.;
/// Height of a full bar.
pub(crate) const BAR_H: f32 = 10.;
pub(crate) const MEDIUM_H: f32 = 8.;
pub(crate) const NARROW_H: f32 = 4.;
/// A mark's side.
pub(crate) const MARK: f32 = 10.;
/// Gap between a bar (or its mark) and the text beside it.
const TEXT_GAP: f32 = 4.;
/// How far a left, right, top or bottom text may run.
const TEXT_W: f32 = 240.;
const TEXT_SIZE: f32 = 10.;

/// One row of the table as it draws for one task: dates resolved to chart
/// pixels, the task's override merged over its looks, texts read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DrawRow {
    pub name: String,
    /// 1 to 4.
    pub band: u8,
    pub x0: f32,
    /// Equal to `x0` for a task with no span (a milestone).
    pub x1: f32,
    pub start: Mark,
    pub middle: Middle,
    pub end: Mark,
    /// `(position, text)` for each position whose field read non-empty.
    pub texts: Vec<(&'static str, String)>,
    /// The colour a part without its own takes: the task's palette colour.
    pub color: u32,
    /// `(y, h)` within the band of the palette bar this row stands in for
    /// (an override over no table): the middle keeps it until the override
    /// names a shape.
    pub palette_box: Option<(f32, f32)>,
}

/// How the palette drew a task's bar: what an override over no table, and a
/// row without a colour of its own, keep (#1696).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaletteLook {
    pub color: u32,
    pub shape: BarShape,
    pub inactive: bool,
}

impl PaletteLook {
    /// The look of `bar` under `palette`; the built-in blue bar without one.
    pub fn of(bar: Option<GanttBar>, palette: Option<&projcore::uiplugin::Palette>) -> Self {
        match bar {
            Some(b) => {
                let (color, shape) = bar_look(b.kind, palette);
                PaletteLook {
                    color,
                    shape,
                    inactive: b.inactive,
                }
            }
            None => PaletteLook {
                color: BRAND,
                shape: BarShape::Bar,
                inactive: false,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkShape {
    None,
    Square,
    Diamond,
    Circle,
    TriangleUp,
    TriangleDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
}

impl MarkShape {
    /// A shape by name; an unknown name is drawn as none.
    pub fn parse(name: Option<&str>) -> MarkShape {
        match name.map(str::trim) {
            Some("square") => MarkShape::Square,
            Some("diamond") => MarkShape::Diamond,
            Some("circle") => MarkShape::Circle,
            Some("triangle-up") => MarkShape::TriangleUp,
            Some("triangle-down") => MarkShape::TriangleDown,
            Some("arrow-left") => MarkShape::ArrowLeft,
            Some("arrow-right") => MarkShape::ArrowRight,
            Some("arrow-up") => MarkShape::ArrowUp,
            Some("arrow-down") => MarkShape::ArrowDown,
            _ => MarkShape::None,
        }
    }

    /// The outline around a centre `(0, 0)` in a [`MARK`] square, clockwise.
    pub fn polygon(self) -> Vec<(f32, f32)> {
        let h = MARK / 2.;
        let arrow_right = [
            (-h, -2.),
            (1., -2.),
            (1., -h),
            (h, 0.),
            (1., h),
            (1., 2.),
            (-h, 2.),
        ];
        match self {
            MarkShape::None => Vec::new(),
            MarkShape::Square => vec![(-h, -h), (h, -h), (h, h), (-h, h)],
            MarkShape::Diamond => vec![(0., -h), (h, 0.), (0., h), (-h, 0.)],
            MarkShape::Circle => (0..16)
                .map(|k| {
                    let a = k as f32 / 16. * std::f32::consts::TAU;
                    (h * a.cos(), h * a.sin())
                })
                .collect(),
            MarkShape::TriangleUp => vec![(0., -h), (h, h), (-h, h)],
            MarkShape::TriangleDown => vec![(-h, -h), (h, -h), (0., h)],
            MarkShape::ArrowRight => arrow_right.to_vec(),
            MarkShape::ArrowLeft => arrow_right.iter().map(|&(x, y)| (-x, y)).collect(),
            MarkShape::ArrowUp => arrow_right.iter().map(|&(x, y)| (y, -x)).collect(),
            MarkShape::ArrowDown => arrow_right.iter().map(|&(x, y)| (-y, x)).collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkKind {
    Solid,
    /// The outline only, dashed.
    Dashed,
    /// Filled, with a solid outline.
    Framed,
}

impl MarkKind {
    pub fn parse(name: Option<&str>) -> MarkKind {
        match name.map(str::trim) {
            Some("dashed") => MarkKind::Dashed,
            Some("framed") => MarkKind::Framed,
            _ => MarkKind::Solid,
        }
    }
}

/// The middle bar's height and top within the bar zone.
pub(crate) fn middle_geometry(shape: Option<&str>) -> (f32, f32) {
    let (h, align) = match shape.map(str::trim) {
        Some("medium-top") => (MEDIUM_H, 0),
        Some("medium-middle") => (MEDIUM_H, 1),
        Some("medium-bottom") => (MEDIUM_H, 2),
        Some("narrow-top") => (NARROW_H, 0),
        Some("narrow-middle") => (NARROW_H, 1),
        Some("narrow-bottom") => (NARROW_H, 2),
        _ => (BAR_H, 0),
    };
    let y = match align {
        0 => 0.,
        1 => (BAR_H - h) / 2.,
        _ => BAR_H - h,
    };
    (y, h)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Center,
    Right,
}

/// One thing to draw, in strip pixels (`y` includes the band's offset).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Part {
    Middle {
        band: u8,
        name: String,
        rect: (f32, f32, f32, f32),
        color: u32,
        pattern: Option<String>,
    },
    Mark {
        band: u8,
        rect: (f32, f32, f32, f32),
        shape: MarkShape,
        kind: MarkKind,
        color: u32,
    },
    Text {
        band: u8,
        pos: &'static str,
        text: String,
        rect: (f32, f32, f32, f32),
        align: Align,
        /// The middle's colour under an inside text, for its contrast.
        on: Option<u32>,
    },
}

impl Part {
    pub fn band(&self) -> u8 {
        match self {
            Part::Middle { band, .. } | Part::Mark { band, .. } | Part::Text { band, .. } => *band,
        }
    }
}

/// The parts of `rows`, in table order: for each row its middle, start and
/// end marks, then its texts.
pub(crate) fn bar_parts(rows: &[DrawRow]) -> Vec<Part> {
    let mut parts = Vec::new();
    for row in rows {
        let band_y = f32::from(row.band.saturating_sub(1)) * ROW_H;
        let zone = band_y + BAR_Y;
        let (dy, h) = match (&row.middle.shape, row.palette_box) {
            (None, Some((y, h))) => (y - BAR_Y, h),
            _ => middle_geometry(row.middle.shape.as_deref()),
        };
        let color = row.middle.color.unwrap_or(row.color);
        parts.push(Part::Middle {
            band: row.band,
            name: row.name.clone(),
            rect: (row.x0, zone + dy, (row.x1 - row.x0).max(0.), h),
            color,
            pattern: row.middle.pattern.clone(),
        });
        let mut edge_w = [0.; 2];
        for (k, (mark, x)) in [(&row.start, row.x0), (&row.end, row.x1)]
            .into_iter()
            .enumerate()
        {
            let shape = MarkShape::parse(mark.shape.as_deref());
            if shape == MarkShape::None {
                continue;
            }
            edge_w[k] = MARK / 2.;
            parts.push(Part::Mark {
                band: row.band,
                rect: (x - MARK / 2., zone + (BAR_H - MARK) / 2., MARK, MARK),
                shape,
                kind: MarkKind::parse(mark.kind.as_deref()),
                color: mark.color.unwrap_or(row.color),
            });
        }
        let mid = (row.x0 + row.x1) / 2.;
        for (pos, text) in &row.texts {
            let (rect, align, on) = match *pos {
                "left" => (
                    (row.x0 - edge_w[0] - TEXT_GAP - TEXT_W, zone, TEXT_W, BAR_H),
                    Align::Right,
                    None,
                ),
                "right" => (
                    (row.x1 + edge_w[1] + TEXT_GAP, zone, TEXT_W, BAR_H),
                    Align::Left,
                    None,
                ),
                "top" => (
                    (mid - TEXT_W / 2., band_y, TEXT_W, BAR_Y),
                    Align::Center,
                    None,
                ),
                "bottom" => (
                    (
                        mid - TEXT_W / 2.,
                        zone + BAR_H,
                        TEXT_W,
                        ROW_H - BAR_Y - BAR_H,
                    ),
                    Align::Center,
                    None,
                ),
                _ => (
                    (row.x0, zone, (row.x1 - row.x0).max(0.), BAR_H),
                    Align::Center,
                    Some(color),
                ),
            };
            parts.push(Part::Text {
                band: row.band,
                pos,
                text: text.clone(),
                rect,
                align,
                on,
            });
        }
    }
    parts
}

/// The first row's middle rectangle `(x, y, w, h)` in strip pixels: what the
/// plugin overlay's `OverlayRow.bar` follows.
pub(crate) fn first_middle(rows: &[DrawRow]) -> Option<(f32, f32, f32, f32)> {
    bar_parts(rows.get(..1)?).into_iter().find_map(|p| match p {
        Part::Middle { rect, .. } => Some(rect),
        _ => None,
    })
}

/// Whether `task` is drawn by the rows: the table has some, or the task has
/// an override over a plain leaf bar.
fn engaged(f: &projcore::formatting::Formatting, task: &Task) -> bool {
    !f.bars.is_empty() || (f.task_bar(task.uid).is_some() && !task.summary && !task.is_milestone())
}

/// What a task's bar styles draw: its parts, and whether the palette's
/// baseline and leveling-delay marks stay (an override over a plain leaf
/// bar; a table replaces them).
pub(crate) struct BarStyled {
    pub parts: Vec<Part>,
    pub marks: bool,
}

/// `task`'s [`BarStyled`] under the plan, or `None` when the palette drawing
/// applies.
pub(crate) fn bar_styled(
    ed: &ProjectEditor,
    task: &Task,
    scale: GanttScale,
    look: PaletteLook,
) -> Option<BarStyled> {
    let rows = bar_rows(ed, task, scale, look)?;
    Some(BarStyled {
        parts: bar_parts(&rows),
        marks: ed.project().formatting.bars.is_empty(),
    })
}

/// `task`'s rows as drawn under the plan's bar styles, or `None` when the
/// palette drawing applies (no table, and no override of a plain leaf bar).
/// `look` is how the palette drew the task. Rows whose From or To date the
/// task does not have draw nothing. An override only changes a row's looks;
/// over no table it restyles the palette's own bar, which keeps its place
/// and size until the override says otherwise.
pub(crate) fn bar_rows(
    ed: &ProjectEditor,
    task: &Task,
    scale: GanttScale,
    look: PaletteLook,
) -> Option<Vec<DrawRow>> {
    let f = &ed.project().formatting;
    if !engaged(f, task) || task.is_null {
        return None;
    }
    let over = f.task_bar(task.uid);
    let reader = FieldReader::new(ed);
    let synthesized;
    let rows: Vec<&BarRow> = if f.bars.is_empty() {
        let mut row = BarRow::default();
        if look.inactive || look.shape == BarShape::Hollow {
            row.middle.pattern = Some("hollow".into());
        }
        synthesized = row;
        vec![&synthesized]
    } else {
        let facts = ed.bar_facts(&reader, task, f.bars_use_flags());
        f.bars_for(&facts)
    };
    let palette_box = f.bars.is_empty().then_some(match look.shape {
        BarShape::Thin => (11., 6.),
        _ => (5., 14.),
    });
    let day = |d: projcore::DateTime| (d.day_number() - scale.origin_day) as f32;
    // The task bar's own whole-day frame, which a CompleteThrough end is a
    // fraction of (#1696).
    let frame = ed
        .disp_start(task.uid)
        .zip(ed.disp_finish(task.uid))
        .map(|(s, f)| (day(s) * scale.day_w, (day(f) + 1.) * scale.day_w));
    let through = ed.bar_through(&reader, task);
    let at = |d: projcore::DateTime, kind: BarDateKind| match kind {
        BarDateKind::Start => Some(day(d) * scale.day_w),
        BarDateKind::Finish => Some((day(d) + 1.) * scale.day_w),
        BarDateKind::Instant => Some(instant_x(scale, 0., d)),
        BarDateKind::Through => {
            let ((fs, fe), f) = frame.zip(through)?;
            Some(fs + f as f32 * (fe - fs))
        }
    };
    let mut out = Vec::new();
    for row in rows {
        let (Some(from), Some(to)) = (
            ed.bar_date(&reader, task, &row.from, Field::Start),
            ed.bar_date(&reader, task, &row.to, Field::Finish),
        ) else {
            continue;
        };
        // Whole days for the start and finish fields (the palette's bars),
        // a fraction of the bar for CompleteThrough, the instant itself for
        // the others. Only a milestone sits in the middle of its day, as the
        // palette's diamond does; any other row without a span is a point at
        // its date.
        let (x0, x1) = if task.is_milestone() && to <= from {
            let mid = (day(from) + 0.5) * scale.day_w;
            (mid, mid)
        } else {
            let (Some(x0), Some(x1)) = (
                at(from, BarDateKind::of(&row.from, Field::Start)),
                at(to, BarDateKind::of(&row.to, Field::Finish)),
            ) else {
                continue;
            };
            (x0, x1.max(x0))
        };
        let text = match over {
            Some(o) => row.text.clone().over(&o.text),
            None => row.text.clone(),
        };
        out.push(DrawRow {
            name: row.name.clone(),
            band: row.row.clamp(1, projcore::formatting::bars::MAX_ROW),
            x0,
            x1,
            start: over.map_or_else(|| row.start.clone(), |o| row.start.clone().over(&o.start)),
            middle: over.map_or_else(
                || row.middle.clone(),
                |o| row.middle.clone().over(&o.middle),
            ),
            end: over.map_or_else(|| row.end.clone(), |o| row.end.clone().over(&o.end)),
            texts: texts_of(&text)
                .into_iter()
                .filter_map(|(pos, field)| {
                    let s = ed.bar_text(&reader, task, field);
                    (!s.is_empty()).then_some((pos, s))
                })
                .collect(),
            color: look.color,
            palette_box,
        });
    }
    Some(out)
}

fn texts_of(text: &BarText) -> Vec<(&'static str, &str)> {
    text.positions()
        .into_iter()
        .filter_map(|(pos, field)| Some((pos, field?)))
        .collect()
}

/// The harness's `bars_drawn` entries for a task: `<uid>:<row>:<name>` for
/// each row that draws, in table order.
pub(crate) fn drawn_entries(uid: i32, rows: &[DrawRow]) -> Vec<String> {
    rows.iter()
        .map(|r| format!("{uid}:{}:{}", r.band, r.name))
        .collect()
}

/// The dashes of `polygon`'s closed outline: `dash` px drawn, `gap` px left,
/// restarting at each corner so every corner is inked.
pub(crate) fn dash_edges(
    polygon: &[(f32, f32)],
    dash: f32,
    gap: f32,
) -> Vec<((f32, f32), (f32, f32))> {
    let mut out = Vec::new();
    for k in 0..polygon.len() {
        let (a, b) = (polygon[k], polygon[(k + 1) % polygon.len()]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        if len <= 0. {
            continue;
        }
        let (ux, uy) = (dx / len, dy / len);
        let mut t = 0.;
        while t < len {
            let end = (t + dash).min(len);
            out.push((
                (a.0 + ux * t, a.1 + uy * t),
                (a.0 + ux * end, a.1 + uy * end),
            ));
            t += dash + gap;
        }
    }
    out
}

/// Black or white, whichever reads on `color`.
fn contrast(color: u32) -> Hsla {
    let (r, g, b) = (
        f32::from(((color >> 16) & 0xFF) as u8),
        f32::from(((color >> 8) & 0xFF) as u8),
        f32::from((color & 0xFF) as u8),
    );
    let luma = 0.299 * r + 0.587 * g + 0.114 * b;
    hsla_u(if luma > 140. { 0x000000 } else { 0xFFFFFF })
}

fn mark_element(shape: MarkShape, kind: MarkKind, color: u32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let (cx, cy) = (
                f32::from(b.origin.x) + f32::from(b.size.width) / 2.,
                f32::from(b.origin.y) + f32::from(b.size.height) / 2.,
            );
            let at = |(x, y): (f32, f32)| point(px(cx + x), px(cy + y));
            let polygon = shape.polygon();
            if polygon.len() < 3 {
                return;
            }
            if kind != MarkKind::Dashed {
                let mut path = PathBuilder::fill();
                path.move_to(at(polygon[0]));
                for p in &polygon[1..] {
                    path.line_to(at(*p));
                }
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, rgb(color));
                }
            }
            let stroke = |window: &mut Window, a: (f32, f32), z: (f32, f32), c: u32| {
                let mut path = PathBuilder::stroke(px(1.));
                path.move_to(at(a));
                path.line_to(at(z));
                if let Ok(path) = path.build() {
                    window.paint_path(path, rgb(c));
                }
            };
            match kind {
                MarkKind::Solid => {}
                MarkKind::Framed => {
                    for k in 0..polygon.len() {
                        stroke(
                            window,
                            polygon[k],
                            polygon[(k + 1) % polygon.len()],
                            0x000000,
                        );
                    }
                }
                MarkKind::Dashed => {
                    for (a, z) in dash_edges(&polygon, 3., 2.) {
                        stroke(window, a, z, color);
                    }
                }
            }
        },
    )
    .size_full()
}

fn middle_element(rect: (f32, f32, f32, f32), color: u32, pattern: Option<&str>) -> Div {
    let style = PlanStyle {
        color: Some(color),
        pattern: pattern.map(str::to_string),
        ..PlanStyle::default()
    };
    let hollow = crate::plugin_ui::pattern_look(pattern) == crate::plugin_ui::PatternLook::Hollow;
    let base = crate::plugin_ui::base_fill(&style);
    let el = div()
        .absolute()
        .left(px(rect.0))
        .top(px(rect.1))
        .w(px(rect.2))
        .h(px(rect.3))
        .overflow_hidden();
    let el = if hollow {
        el.border_1().border_color(hsla_u(color))
    } else {
        el.bg(hsla_u(base.unwrap_or(color)))
    };
    el.children(crate::plugin_ui::pattern_layer(&style))
}

/// A bar's right-press handler, shared by its bands.
pub(crate) type OnRight = std::rc::Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// The parts as elements for a strip: per band a container over its middle
/// and marks (the bar's own right-press, and for the first band the
/// `bar:<id>` probe), then the texts, which take no press.
pub(crate) fn styled_children(
    parts: Vec<Part>,
    id: i32,
    pal: Pal,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
    on_right: OnRight,
) -> Vec<AnyElement> {
    let mut out: Vec<AnyElement> = Vec::new();
    let mut bands: Vec<u8> = parts.iter().map(Part::band).collect();
    bands.sort_unstable();
    bands.dedup();
    let mut probed = false;
    for band in bands {
        let solid: Vec<&Part> = parts
            .iter()
            .filter(|p| p.band() == band && !matches!(p, Part::Text { .. }))
            .collect();
        let rect_of = |p: &&Part| match p {
            Part::Middle { rect, .. } | Part::Mark { rect, .. } => *rect,
            Part::Text { rect, .. } => *rect,
        };
        let left = solid
            .iter()
            .map(|p| rect_of(p).0)
            .fold(f32::INFINITY, f32::min);
        let right = solid
            .iter()
            .map(|p| rect_of(p).0 + rect_of(p).2)
            .fold(f32::NEG_INFINITY, f32::max);
        if !left.is_finite() {
            continue;
        }
        let band_y = f32::from(band - 1) * ROW_H;
        let handler = on_right.clone();
        let mut container = div()
            .absolute()
            .left(px(left))
            .top(px(band_y))
            .w(px((right - left).max(1.)))
            .h(px(ROW_H))
            .on_mouse_down(MouseButton::Right, move |e, w, a| handler(e, w, a));
        for part in solid {
            container = match part {
                Part::Middle {
                    rect,
                    color,
                    pattern,
                    ..
                } => {
                    if rect.2 <= 0. {
                        continue;
                    }
                    let local = (rect.0 - left, rect.1 - band_y, rect.2, rect.3);
                    container.child(middle_element(local, *color, pattern.as_deref()))
                }
                Part::Mark {
                    rect,
                    shape,
                    kind,
                    color,
                    ..
                } => container.child(
                    div()
                        .absolute()
                        .left(px(rect.0 - left))
                        .top(px(rect.1 - band_y))
                        .size(px(rect.2))
                        .child(mark_element(*shape, *kind, *color)),
                ),
                Part::Text { .. } => container,
            };
        }
        if !probed {
            probed = true;
            container = container.child(probe(probes, format!("bar:{id}")));
        }
        out.push(container.into_any_element());
    }
    for part in &parts {
        let Part::Text {
            text,
            rect,
            align,
            on,
            ..
        } = part
        else {
            continue;
        };
        if rect.2 <= 0. {
            continue;
        }
        let el = div()
            .absolute()
            .left(px(rect.0))
            .top(px(rect.1))
            .w(px(rect.2))
            .h(px(rect.3))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(px(TEXT_SIZE))
            .line_height(px(rect.3))
            .text_color(on.map_or(pal.fg, contrast))
            .child(text.clone());
        out.push(
            match align {
                Align::Left => el,
                Align::Center => el.text_center(),
                Align::Right => el.text_right(),
            }
            .into_any_element(),
        );
    }
    out
}

#[cfg(test)]
mod tests;
