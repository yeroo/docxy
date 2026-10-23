use super::*;
use crate::json::Json;

fn json(s: &str) -> Json {
    Json::parse(s).unwrap()
}

fn applied(s: &str) -> Layout {
    let mut l = Layout::default();
    l.apply(Some(&json(s))).unwrap();
    l
}

#[test]
fn default_is_empty_and_the_formatting_stays_empty() {
    assert!(Layout::default().is_empty());
    assert!(Formatting::default().is_empty());
    assert!(Layout::default().rounds_bars());
    assert!(!Layout::default().rolls_up());
    assert_eq!(Layout::default().bar_height_px(), None);
    let mut f = Formatting::default();
    f.layout.bar_height = Some(12);
    assert!(!f.is_empty());
}

#[test]
fn json_round_trips_all_seven_keys() {
    let l = applied(
        r#"{"bar_height":18,"rollup":true,"hide_rollup_expanded":false,"round_bars":false,
            "show_splits":true,"show_drawings":false,"bar_text_date_format":"1/28"}"#,
    );
    assert_eq!(l.bar_height, Some(18));
    assert_eq!(l.round_bars, Some(false));
    assert_eq!(l.bar_text_date_format.as_deref(), Some("1/28"));
    let j = l.to_json();
    assert!(matches!(&j, Json::Obj(p) if p.len() == 7));
    let mut again = Layout::default();
    again.apply(Some(&j)).unwrap();
    assert_eq!(again, l);
    let Json::Obj(p) = Layout::default().to_json() else {
        panic!()
    };
    assert!(p.iter().all(|(_, v)| *v == Json::Null));
}

#[test]
fn partial_apply_and_null_clear() {
    let mut l = applied(r#"{"bar_height":10,"rollup":true}"#);
    l.apply(Some(&json(r#"{"show_splits":false,"bogus":3}"#)))
        .unwrap();
    assert_eq!(l.bar_height, Some(10));
    assert_eq!(l.rollup, Some(true));
    assert_eq!(l.show_splits, Some(false));
    l.apply(Some(&json(r#"{"rollup":null}"#))).unwrap();
    assert_eq!(l.rollup, None);
    assert_eq!(l.bar_height, Some(10));
    l.apply(None).unwrap();
    assert_eq!(l.bar_height, Some(10));
    l.apply(Some(&Json::Null)).unwrap();
    assert!(l.is_empty());
}

#[test]
fn bar_height_clamps_and_unset_differs_from_twelve() {
    assert_eq!(applied(r#"{"bar_height":2}"#).bar_height, Some(6));
    assert_eq!(applied(r#"{"bar_height":99}"#).bar_height, Some(24));
    assert_eq!(applied(r#"{"bar_height":12}"#).bar_height_px(), Some(16.));
    assert_eq!(applied(r#"{"bar_height":6}"#).bar_height_px(), Some(8.));
    assert_eq!(points_to_px(10) , 40. / 3.);
    assert_ne!(applied(r#"{"bar_height":12}"#), Layout::default());
}

#[test]
fn wrong_types_name_the_key() {
    let mut l = Layout::default();
    let err = |l: &mut Layout, s: &str| l.apply(Some(&json(s))).unwrap_err();
    assert_eq!(
        err(&mut l, r#"{"bar_height":"tall"}"#),
        "view.layout.bar_height is not a number or null"
    );
    assert_eq!(
        err(&mut l, r#"{"rollup":1}"#),
        "view.layout.rollup is not a boolean or null"
    );
    assert_eq!(
        err(&mut l, r#"{"bar_text_date_format":true}"#),
        "view.layout.bar_text_date_format is not a string or null"
    );
    assert_eq!(
        l.apply(Some(&json("[1]"))).unwrap_err(),
        "view.layout is not an object or null"
    );
}

#[test]
fn a_bad_key_after_a_good_one_changes_nothing() {
    let mut f = Formatting::default();
    f.layout.bar_height = Some(8);
    let before = f.clone();
    let view = json(r#"{"layout":{"bar_height":20,"rollup":"yes"}}"#);
    assert!(f.apply_view(&view).is_err());
    assert_eq!(f, before);
}

#[test]
fn apply_view_reports_a_change_once() {
    let mut f = Formatting::default();
    let view = json(r#"{"layout":{"bar_height":12,"round_bars":false}}"#);
    assert!(f.apply_view(&view).unwrap());
    assert!(!f.apply_view(&view).unwrap());
    assert!(f.apply_view(&json(r#"{"layout":null}"#)).unwrap());
    assert!(f.is_empty());
}

#[test]
fn xml_round_trips_and_an_old_part_is_unchanged() {
    let mut f = Formatting::default();
    f.link_style = Some("rectilinear".into());
    let old = f.to_xml();
    assert!(!old.contains("<layout"));
    assert_eq!(Formatting::from_xml(&old).unwrap().to_xml(), old);

    f.layout = applied(
        r#"{"bar_height":14,"rollup":true,"hide_rollup_expanded":true,"round_bars":false,
            "show_splits":false,"show_drawings":true,"bar_text_date_format":"A <\"b\">"}"#,
    );
    let xml = f.to_xml();
    assert_eq!(xml.matches("<layout").count(), 1);
    assert!(xml.contains(
        r#"<layout bar-height="14" rollup="1" hide-rollup-expanded="1" round-bars="0" show-splits="0" show-drawings="1" bar-text-date-format="A &lt;&quot;b&quot;&gt;"/>"#
    ));
    let back = Formatting::from_xml(&xml).unwrap();
    assert_eq!(back, f);
    assert_eq!(back.to_xml(), xml);
}

#[test]
fn xml_reads_leniently() {
    let xml = r#"<?xml version="1.0"?><formatting><layout bar-height="wide" rollup="maybe"
        round-bars="0" future="x"/></formatting>"#;
    let f = Formatting::from_xml(xml).unwrap();
    assert_eq!(f.layout.bar_height, None);
    assert_eq!(f.layout.rollup, None);
    assert_eq!(f.layout.round_bars, Some(false));
    let big = r#"<formatting><layout bar-height="400"/></formatting>"#;
    assert_eq!(Formatting::from_xml(big).unwrap().layout.bar_height, Some(24));
}

#[test]
fn a_layout_command_is_one_undo_step_and_an_unchanged_one_is_none() {
    use crate::editor::Editor;
    use crate::model::Project;
    let mut ed = Editor::new(Project::default());
    let depth = ed.undo_depth();
    let view = json(r#"{"layout":{"bar_height":18,"rollup":true}}"#);
    ed.edit_formatting(|f| {
        f.apply_view(&view).unwrap();
    });
    assert_eq!(ed.undo_depth(), depth + 1);
    assert_eq!(ed.project().formatting.layout.bar_height, Some(18));
    ed.edit_formatting(|f| {
        assert!(!f.apply_view(&view).unwrap());
    });
    assert_eq!(ed.undo_depth(), depth + 1);
    assert!(ed.undo());
    assert!(ed.project().formatting.is_empty());
    assert!(ed.redo());
    assert_eq!(ed.project().formatting.layout.rollup, Some(true));
}
