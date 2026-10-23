//! A summary's rolled-up children (#1695): with Layout's "Always roll up
//! Gantt bars" on, the summary row also draws its leaf tasks' bars and its
//! milestones. Pure: [`rolled_parts`] reads the plan, the chart paints.
use super::gantt::{BarKind, GanttBar, GanttScale, bar_rect, gantt_bar};
use projcore::editor::Editor;
use projcore::model::Task;

/// One child drawn on its summary's row: its kind (for the colour), and the
/// x and width its own bar has in the chart. A milestone's `x` is its
/// diamond's centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RolledChild {
    pub uid: i32,
    pub kind: BarKind,
    pub x: f32,
    pub w: f32,
}

/// Whether `summary`'s row shows its children now: roll-up on, and the
/// summary collapsed or, expanded, not hidden by "Hide rollup bars when
/// summary expanded".
pub(crate) fn rolls_up_now(ed: &Editor, summary: &Task) -> bool {
    let layout = &ed.project().formatting.layout;
    summary.summary
        && layout.rolls_up()
        && (ed.is_collapsed(summary.uid) || layout.hide_rollup_expanded != Some(true))
}

/// The children to draw on `summary`'s row: `None` when roll-up is off (or
/// hidden while expanded), else every descendant leaf and milestone that
/// is active and has a bar, in outline order. Nested summaries recurse and are
/// never drawn themselves; a filtered-out descendant still rolls up. `style`
/// is the view's Gantt Chart Format toggles (`ProjectView::styled_bar`).
pub(crate) fn rolled_parts(
    ed: &Editor,
    summary: &Task,
    scale: GanttScale,
    style: impl Fn(GanttBar) -> GanttBar,
) -> Option<Vec<RolledChild>> {
    if !rolls_up_now(ed, summary) {
        return None;
    }
    let tasks = &ed.project().tasks;
    let at = tasks.iter().position(|t| t.uid == summary.uid)?;
    let children = tasks[at + 1..]
        .iter()
        .take_while(|t| t.outline_level > summary.outline_level)
        .filter(|t| !t.summary)
        .filter_map(|t| {
            let bar = style(gantt_bar(ed, t, scale)?);
            if bar.inactive {
                return None;
            }
            let (x, _, w, _) = bar_rect(&bar, scale.day_w);
            Some(match bar.kind {
                BarKind::Milestone => RolledChild {
                    uid: t.uid,
                    kind: bar.kind,
                    x: x + w / 2.,
                    w,
                },
                kind => RolledChild {
                    uid: t.uid,
                    kind,
                    x,
                    w,
                },
            })
        })
        .collect();
    Some(children)
}
