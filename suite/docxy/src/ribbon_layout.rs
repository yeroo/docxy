//! Where each ribbon group's content landed, against the group (#1018).
//!
//! The ribbon fits three small-button rows. A static rule keeps the tables
//! honest (`sheet_ribbon::col`, `ribbonspec::column`), and this is the measured
//! half: while a ribbon draws, each group records its own bounds (`ribbon-group:`),
//! its title row (`ribbon-title:`) and the columns / row stacks inside it
//! (`ribbon-content:`), the way the title bar records its regions. `ribbon-layout`
//! reports them, and a debug build warns when a group's content does not fit.

use crate::Probes;
use ctlcore::json::Json;
use gpui::{Bounds, Pixels};

/// A rounding tolerance, in logical pixels.
const EPS: f32 = 0.5;

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

/// Whether `measured` is a finished frame of the tab whose groups are `titles`:
/// something was measured, and nothing from another tab's groups.
pub(crate) fn is_frame_of(measured: &[GroupLayout], titles: &[&str]) -> bool {
    !measured.is_empty() && measured.iter().all(|g| titles.contains(&g.title.as_str()))
}

/// The `ribbon-layout` reply while the frame on record is not the shown tab's:
/// no groups yet, ask again after a frame.
pub(crate) fn unsettled_json(tab: &str) -> Json {
    Json::obj(vec![
        ("tab", Json::Str(tab.into())),
        ("settled", Json::Bool(false)),
        ("any_clipped_v", Json::Bool(false)),
        ("any_clipped_h", Json::Bool(false)),
        ("groups", Json::Arr(Vec::new())),
    ])
}

/// The `ribbon-layout` reply for the groups of the tab shown. A group the
/// ribbon dropped (responsive collapse) is listed `hidden`.
pub(crate) fn layout_json(tab: &str, titles: &[&str], measured: &[GroupLayout]) -> Json {
    let groups: Vec<Json> = titles
        .iter()
        .map(|title| {
            let Some(g) = measured.iter().find(|g| g.title == *title) else {
                return Json::obj(vec![
                    ("title", Json::Str((*title).into())),
                    ("hidden", Json::Bool(true)),
                    ("bounds", Json::Null),
                    ("content_bounds", Json::Null),
                    ("clipped_v", Json::Bool(false)),
                    ("clipped_h", Json::Bool(false)),
                ]);
            };
            Json::obj(vec![
                ("title", Json::Str(g.title.clone())),
                ("hidden", Json::Bool(false)),
                ("bounds", rect_json(g.bounds)),
                ("content_bounds", g.content.map_or(Json::Null, rect_json)),
                ("clipped_v", Json::Bool(g.clipped_v)),
                ("clipped_h", Json::Bool(g.clipped_h)),
            ])
        })
        .collect();
    let any = |key: &str| Json::Bool(groups.iter().any(|g| g.get(key) == Some(&Json::Bool(true))));
    Json::obj(vec![
        ("tab", Json::Str(tab.into())),
        ("settled", Json::Bool(true)),
        ("any_clipped_v", any("clipped_v")),
        ("any_clipped_h", any("clipped_h")),
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
        let json = layout_json("Home", &["Editing"], &five);
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
            layout_json("Home", &["Editing"], &m).get("settled"),
            Some(&Json::Bool(true))
        );
    }

    #[test]
    fn a_dropped_group_is_listed_hidden() {
        let json = layout_json("Home", &["Editing"], &[]);
        let g = &json.get("groups").unwrap().as_array().unwrap()[0];
        assert_eq!(g.get("hidden"), Some(&Json::Bool(true)));
    }
}
