//! Where each ribbon group's content landed, against the group (#1018).
//!
//! The ribbon fits three small-button rows. A static rule keeps the tables
//! honest (`sheet_ribbon::col`, `ribbonspec::column`), and this is the measured
//! half: while a ribbon draws, each group records its own bounds (`ribbon-group:`),
//! its title row (`ribbon-title:`) and the columns / row stacks inside it
//! (`ribbon-content:`), the way the title bar records its regions. `ribbon-layout`
//! reports them, and a debug build warns when a group's content does not fit.
//!
//! Since #1020 the ribbon fits the window (`ribbon_fit`), and the report says
//! where each group went (`state`, `in_overflow`), checks every drawn group
//! against the ribbon's own edges (`ribbon-body:`; a group past the window's
//! edge is clipped, not merely one whose content spills out of it), and sets
//! each group's drawn width beside the width the fit assumed (`estimate`).

use crate::Probes;
use crate::ribbon_fit::State;
use ctlcore::json::Json;
use gpui::{Bounds, Pixels};

/// A rounding tolerance, in logical pixels.
const EPS: f32 = 0.5;

/// How far a drawn group's width may be from its estimate (#1020, #357).
pub(crate) const ESTIMATE_TOL: f32 = 6.0;

/// Where the fit put one group of the tab shown, and the width it assumed.
pub(crate) struct GroupFit<'a> {
    pub title: &'a str,
    pub state: State,
    /// Listed in the overflow chevron rather than drawn.
    pub in_overflow: bool,
    pub estimate: f32,
}

/// One group's measured layout. `content` is the union of its columns and row
/// stacks, `None` for a group made of buttons that size to the group.
pub(crate) struct GroupLayout {
    pub title: String,
    pub bounds: Bounds<Pixels>,
    pub content: Option<Bounds<Pixels>>,
    pub clipped_v: bool,
    pub clipped_h: bool,
}

fn edges(b: Bounds<Pixels>) -> (f32, f32, f32, f32) {
    (
        f32::from(b.origin.x),
        f32::from(b.origin.y),
        f32::from(b.origin.x + b.size.width),
        f32::from(b.origin.y + b.size.height),
    )
}

/// Whether the content spills out of the group vertically and horizontally.
/// Vertically it must sit above the title row and inside the group, and the
/// title row must stay inside the group: a column that is too tall pushes the
/// title down as well as growing past it.
pub(crate) fn clipped(
    group: Bounds<Pixels>,
    title: Option<Bounds<Pixels>>,
    content: Bounds<Pixels>,
) -> (bool, bool) {
    let (gl, gt, gr, gb) = edges(group);
    let (cl, ct, cr, cb) = edges(content);
    let mut v = ct < gt - EPS || cb > gb + EPS;
    if let Some(title) = title {
        let (_, tt, _, tb) = edges(title);
        v |= cb > tt + EPS || tb > gb + EPS;
    }
    (v, cl < gl - EPS || cr > gr + EPS)
}

/// Every ribbon group in `probes` (a finished frame), in drawn order.
pub(crate) fn measure(probes: &[(String, Bounds<Pixels>)]) -> Vec<GroupLayout> {
    let find = |name: &str| probes.iter().find(|(n, _)| n == name).map(|(_, b)| *b);
    probes
        .iter()
        .filter_map(|(name, bounds)| {
            let title = name.strip_prefix("ribbon-group:")?;
            let prefix = format!("ribbon-content:{title}:");
            let content = probes
                .iter()
                .filter(|(n, _)| n.starts_with(&prefix))
                .map(|(_, b)| *b)
                .reduce(|a, b| a.union(&b));
            let (clipped_v, clipped_h) = content.map_or((false, false), |c| {
                clipped(*bounds, find(&format!("ribbon-title:{title}")), c)
            });
            Some(GroupLayout {
                title: title.to_string(),
                bounds: *bounds,
                content,
                clipped_v,
                clipped_h,
            })
        })
        .collect()
}

fn rect_json(b: Bounds<Pixels>) -> Json {
    let (l, t, r, bt) = edges(b);
    Json::obj(vec![
        ("x", Json::Num(l as f64)),
        ("y", Json::Num(t as f64)),
        ("w", Json::Num((r - l) as f64)),
        ("h", Json::Num((bt - t) as f64)),
    ])
}

/// The tab the finished frame drew, from its `ribbon-tab:` probe: two tabs can
/// share group titles (Task's and Project's Schedule), so titles alone cannot
/// say whose frame it is.
pub(crate) fn shown_tab(probes: &[(String, Bounds<Pixels>)]) -> Option<&str> {
    probes
        .iter()
        .find_map(|(n, _)| n.strip_prefix("ribbon-tab:"))
}

/// Whether `measured` is a finished frame of the tab whose groups are `titles`:
/// something was measured, and nothing from another tab's groups.
pub(crate) fn is_frame_of(measured: &[GroupLayout], titles: &[&str]) -> bool {
    !measured.is_empty() && measured.iter().all(|g| titles.contains(&g.title.as_str()))
}

/// The fit a frame drew the ribbon with: the width fitted into, and where
/// each group went (`ribbon_fit::Fit`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawnFit {
    pub width: f32,
    pub states: Vec<State>,
    pub overflow: Vec<usize>,
}

/// Whether the finished frame was drawn with the fit of now: the same width
/// and every group in the same state. A resize that moves no group between
/// collapsed and drawn (Full to IconOnly, or none at all) changes no probe,
/// so the probes alone cannot tell such a frame from a fresh one.
pub(crate) fn drawn_matches(drawn: Option<&DrawnFit>, width: f32, fits: &[GroupFit]) -> bool {
    drawn.is_some_and(|d| {
        (d.width - width).abs() <= EPS
            && d.states.len() == fits.len()
            && fits
                .iter()
                .enumerate()
                .all(|(i, f)| d.states[i] == f.state && d.overflow.contains(&i) == f.in_overflow)
    })
}

/// Whether the finished frame drew the groups as `fits` places them: every
/// drawn group measured and every chevron group not, a collapsed group as
/// its button (`ribbon-collapsed:`) and no other group so. A frame from
/// before a resize is not the layout `fits` describes.
pub(crate) fn frame_matches(probes: &[(String, Bounds<Pixels>)], fits: &[GroupFit]) -> bool {
    let has = |name: String| probes.iter().any(|(n, _)| *n == name);
    fits.iter().all(|f| {
        has(format!("ribbon-group:{}", f.title)) != f.in_overflow
            && has(format!("ribbon-collapsed:{}", f.title))
                == (f.state == State::Collapsed && !f.in_overflow)
    })
}

/// The ribbon's own bounds in the finished frame (`ribbon-body`).
pub(crate) fn body(probes: &[(String, Bounds<Pixels>)]) -> Option<Bounds<Pixels>> {
    probes
        .iter()
        .find(|(n, _)| n == "ribbon-body")
        .map(|(_, b)| *b)
}

/// The `ribbon-layout` reply while the frame on record is not the shown tab's:
/// no groups yet, ask again after a frame.
pub(crate) fn unsettled_json(tab: &str) -> Json {
    Json::obj(vec![
        ("tab", Json::Str(tab.into())),
        ("settled", Json::Bool(false)),
        ("any_clipped", Json::Bool(false)),
        ("any_clipped_v", Json::Bool(false)),
        ("any_clipped_h", Json::Bool(false)),
        ("estimates_ok", Json::Bool(false)),
        ("groups", Json::Arr(Vec::new())),
    ])
}

/// The `ribbon-layout` reply for the groups of the tab shown, in the order
/// `fits` lists them. A drawn group whose bounds pass the ribbon's left or
/// right edge (`body`) is clipped horizontally: its commands are off screen.
/// A group in the overflow chevron has no bounds and is not clipped.
pub(crate) fn layout_json(
    tab: &str,
    fits: &[GroupFit],
    measured: &[GroupLayout],
    body: Option<Bounds<Pixels>>,
) -> Json {
    let groups: Vec<Json> = fits
        .iter()
        .map(|f| {
            let head = |rest: Vec<(&'static str, Json)>| {
                let mut fields = vec![
                    ("title", Json::Str(f.title.into())),
                    ("state", Json::Str(f.state.name().into())),
                    ("in_overflow", Json::Bool(f.in_overflow)),
                    ("estimate", Json::Num(f.estimate.round() as f64)),
                ];
                fields.extend(rest);
                Json::obj(fields)
            };
            let Some(g) = measured.iter().find(|g| g.title == f.title) else {
                return head(vec![
                    ("bounds", Json::Null),
                    ("content_bounds", Json::Null),
                    ("estimate_ok", Json::Bool(f.in_overflow)),
                    ("clipped_v", Json::Bool(false)),
                    ("clipped_h", Json::Bool(!f.in_overflow)),
                ]);
            };
            let (gl, _, gr, _) = edges(g.bounds);
            let off_ribbon = body.is_some_and(|b| {
                let (bl, _, br, _) = edges(b);
                gl < bl - EPS || gr > br + EPS
            });
            head(vec![
                ("bounds", rect_json(g.bounds)),
                ("content_bounds", g.content.map_or(Json::Null, rect_json)),
                (
                    "estimate_ok",
                    Json::Bool(((gr - gl) - f.estimate).abs() <= ESTIMATE_TOL),
                ),
                ("clipped_v", Json::Bool(g.clipped_v)),
                ("clipped_h", Json::Bool(g.clipped_h || off_ribbon)),
            ])
        })
        .collect();
    let any = |key: &str| groups.iter().any(|g| g.get(key) == Some(&Json::Bool(true)));
    let all = |key: &str| groups.iter().all(|g| g.get(key) == Some(&Json::Bool(true)));
    Json::obj(vec![
        ("tab", Json::Str(tab.into())),
        ("settled", Json::Bool(true)),
        (
            "any_clipped",
            Json::Bool(any("clipped_v") || any("clipped_h")),
        ),
        ("any_clipped_v", Json::Bool(any("clipped_v"))),
        ("any_clipped_h", Json::Bool(any("clipped_h"))),
        ("estimates_ok", Json::Bool(all("estimate_ok"))),
        ("groups", Json::Arr(groups)),
    ])
}

/// The groups of the finished frame whose content is taller than their body,
/// for the debug build's warning (the terminal ribbons check width the same way).
pub(crate) fn warn_clipped(probes: &Probes) -> Vec<String> {
    let mut out = Vec::new();
    for g in measure(&probes.last) {
        if g.clipped_v {
            out.push(g.title);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    fn b(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    /// A 94 px group with a 3 px pad and a 14 px title row: its content area is 74.
    fn frame(content_h: f32) -> Vec<(String, Bounds<Pixels>)> {
        let content_y = 3.0 + (74.0 - content_h) / 2.0;
        vec![
            ("ribbon-group:Editing".into(), b(0., 0., 60., 94.)),
            ("ribbon-title:Editing".into(), b(0., 77., 60., 14.)),
            (
                "ribbon-content:Editing:0".into(),
                b(6., content_y, 40., content_h),
            ),
        ]
    }

    #[test]
    fn three_rows_fit_and_a_fifth_is_clipped_vertically() {
        let fits = measure(&frame(62.));
        assert!(!fits[0].clipped_v && !fits[0].clipped_h);
        let five = measure(&frame(104.));
        assert!(five[0].clipped_v);
        let json = layout_json("Home", &[fit("Editing", State::Full, 60.)], &five, None);
        assert_eq!(json.get("any_clipped_v"), Some(&Json::Bool(true)));
        assert_eq!(json.get("any_clipped_h"), Some(&Json::Bool(false)));
    }

    #[test]
    fn content_wider_than_its_group_is_clipped_horizontally() {
        let mut f = frame(62.);
        f[2].1 = b(6., 9., 80., 62.);
        assert!(measure(&f)[0].clipped_h);
    }

    #[test]
    fn a_title_pushed_out_of_the_group_is_clipped() {
        let mut f = frame(62.);
        f[1].1 = b(0., 90., 60., 14.);
        assert!(measure(&f)[0].clipped_v);
    }

    #[test]
    fn a_group_with_no_column_has_no_content_and_is_not_clipped() {
        let f = frame(62.)[..2].to_vec();
        let m = measure(&f);
        assert!(m[0].content.is_none() && !m[0].clipped_v);
    }

    #[test]
    fn the_frame_names_the_tab_it_drew() {
        let mut f = frame(62.);
        assert_eq!(shown_tab(&f), None);
        f.push(("ribbon-tab:Task".into(), b(0., 0., 400., 100.)));
        assert_eq!(shown_tab(&f), Some("Task"));
    }

    #[test]
    fn a_frame_of_another_tab_or_none_is_not_settled() {
        let m = measure(&frame(62.));
        assert!(is_frame_of(&m, &["Editing", "Cells"]));
        assert!(
            !is_frame_of(&m, &["Cells"]),
            "another tab's group is on record"
        );
        assert!(!is_frame_of(&[], &["Editing"]), "nothing measured yet");
        let json = unsettled_json("Data");
        assert_eq!(json.get("settled"), Some(&Json::Bool(false)));
        assert_eq!(json.get("groups"), Some(&Json::Arr(Vec::new())));
        assert_eq!(
            layout_json("Home", &[fit("Editing", State::Full, 60.)], &m, None).get("settled"),
            Some(&Json::Bool(true))
        );
    }

    fn fit(title: &str, state: State, estimate: f32) -> GroupFit<'_> {
        GroupFit {
            title,
            state,
            in_overflow: false,
            estimate,
        }
    }

    fn group(json: &Json, i: usize) -> &Json {
        &json.get("groups").unwrap().as_array().unwrap()[i]
    }

    /// #1020, issue comment 3: a group drawn whole but past the ribbon's
    /// right edge (off screen) is clipped, though its content fits it.
    #[test]
    fn a_group_past_the_ribbons_edge_is_clipped() {
        let mut f = frame(62.);
        for (_, bounds) in f.iter_mut() {
            bounds.origin.x += px(380.);
        }
        let m = measure(&f);
        assert!(!m[0].clipped_h, "the content fits its group");
        let fits = [fit("Editing", State::Full, 60.)];
        let inside = layout_json("Home", &fits, &m, Some(b(0., 0., 500., 100.)));
        assert_eq!(group(&inside, 0).get("clipped_h"), Some(&Json::Bool(false)));
        assert_eq!(inside.get("any_clipped"), Some(&Json::Bool(false)));
        let narrow = layout_json("Home", &fits, &m, Some(b(0., 0., 400., 100.)));
        assert_eq!(group(&narrow, 0).get("clipped_h"), Some(&Json::Bool(true)));
        assert_eq!(narrow.get("any_clipped"), Some(&Json::Bool(true)));
    }

    #[test]
    fn each_group_reports_its_state_and_estimate() {
        let m = measure(&frame(62.));
        let near = layout_json("Home", &[fit("Editing", State::IconOnly, 64.)], &m, None);
        let g = group(&near, 0);
        assert_eq!(g.get("state"), Some(&Json::Str("icon-only".into())));
        assert_eq!(g.get("estimate"), Some(&Json::Num(64.)));
        assert_eq!(g.get("estimate_ok"), Some(&Json::Bool(true)));
        assert_eq!(near.get("estimates_ok"), Some(&Json::Bool(true)));
        let far = layout_json("Home", &[fit("Editing", State::Full, 80.)], &m, None);
        assert_eq!(far.get("estimates_ok"), Some(&Json::Bool(false)));
    }

    /// A group in the overflow chevron is not drawn: no bounds, and not
    /// clipped. A drawn group with no bounds on record is.
    #[test]
    fn a_chevron_group_has_no_bounds_and_is_not_clipped() {
        let fits = [GroupFit {
            in_overflow: true,
            ..fit("Cells", State::Collapsed, 50.)
        }];
        let json = layout_json("Home", &fits, &[], None);
        let g = group(&json, 0);
        assert_eq!(g.get("in_overflow"), Some(&Json::Bool(true)));
        assert_eq!(g.get("bounds"), Some(&Json::Null));
        assert_eq!(json.get("any_clipped"), Some(&Json::Bool(false)));
        let lost = layout_json("Home", &[fit("Cells", State::Full, 50.)], &[], None);
        assert_eq!(lost.get("any_clipped"), Some(&Json::Bool(true)));
    }

    /// A flyout's copy of a group carries `ribbon-flyout:` probes, which
    /// neither add a group nor move the one drawn in the ribbon.
    #[test]
    fn a_flyouts_probes_are_not_measured() {
        let mut f = frame(62.);
        let plain = measure(&f);
        for (name, bounds) in frame(104.) {
            f.push((format!("{}{name}", crate::FLYOUT_PROBE_PREFIX), bounds));
        }
        let with_flyout = measure(&f);
        assert_eq!(with_flyout.len(), 1);
        assert_eq!(with_flyout[0].bounds, plain[0].bounds);
        assert!(!with_flyout[0].clipped_v);
    }

    /// The frame on record must be the layout the fit describes: a group
    /// drawn in place is not the collapsed button the fit now asks for.
    #[test]
    fn a_frame_from_before_a_resize_does_not_match_the_fit() {
        let f = frame(62.);
        assert!(frame_matches(&f, &[fit("Editing", State::Full, 60.)]));
        assert!(!frame_matches(&f, &[fit("Editing", State::Collapsed, 60.)]));
        let mut collapsed = vec![
            ("ribbon-group:Editing".to_string(), b(0., 0., 60., 94.)),
            ("ribbon-collapsed:Editing".to_string(), b(0., 0., 60., 94.)),
        ];
        assert!(frame_matches(
            &collapsed,
            &[fit("Editing", State::Collapsed, 60.)]
        ));
        assert!(!frame_matches(
            &collapsed,
            &[fit("Editing", State::IconOnly, 60.)]
        ));
        collapsed.clear();
        let chevron = GroupFit {
            in_overflow: true,
            ..fit("Editing", State::Collapsed, 60.)
        };
        assert!(frame_matches(&collapsed, &[chevron]));
    }

    /// #1020 r1: a resize that only drops labels (Full to IconOnly) changes
    /// no probe, so the frame's recorded fit tells it apart: another state or
    /// another width is not the frame of now.
    #[test]
    fn a_frame_drawn_with_another_fit_is_not_settled() {
        let drawn = DrawnFit {
            width: 900.,
            states: vec![State::Full],
            overflow: Vec::new(),
        };
        let full = [fit("Editing", State::Full, 60.)];
        assert!(drawn_matches(Some(&drawn), 900., &full));
        assert!(!drawn_matches(
            Some(&drawn),
            900.,
            &[fit("Editing", State::IconOnly, 40.)]
        ));
        assert!(!drawn_matches(Some(&drawn), 880., &full), "another width");
        assert!(!drawn_matches(None, 900., &full), "nothing drawn");
    }
}
