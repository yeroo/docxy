use super::*;
use crate::design_tab::tests::reopen;
use crate::layout_tab::tests::{ed, ed_mut, three_sections};
use core::prelude::v1::test;
use ctlcore::json::Json;

fn open(t: &mut DocTab, build: fn(&DocTab) -> Result<Dialog, String>) {
    let d = build(t).unwrap();
    t.dialogs.push(d);
}

fn set(t: &mut DocTab, control: &str, value: Json) {
    t.dialogs
        .set(control, &Json::obj(vec![("value", value)]))
        .unwrap_or_else(|e| panic!("{control}: {e}"));
}

fn s(v: &str) -> Json {
    Json::Str(v.into())
}

fn click(t: &mut DocTab, button: &str) -> Result<(), String> {
    crate::dialog_host::dialog_click(t, button)
}

fn control<'a>(t: &'a DocTab, name: &str) -> &'a Control {
    t.dialogs
        .top()
        .unwrap()
        .controls
        .iter()
        .find(|c| c.name == name)
        .unwrap()
}

fn borders(t: &DocTab, k: usize) -> Option<PageBorders> {
    PageBorders::parse(&ed(t).sections()[k])
}

#[test]
fn more_colors_takes_hex_and_refuses_anything_else() {
    let mut t = three_sections();
    open(&mut t, more_colors_dialog);
    assert_eq!(control(&t, "hex").text(), "FFFFFF");
    set(&mut t, "hex", s("12ab"));
    let err = click(&mut t, "OK").unwrap_err();
    assert!(err.contains("hex"), "{err}");
    assert_eq!(t.dialogs.top_id(), "more-colors", "stays open to fix");
    set(&mut t, "hex", s("#12AB9c"));
    click(&mut t, "OK").unwrap();
    assert!(!t.dialogs.is_open());
    assert_eq!(page_background(&t).map(|b| b.color), Some(0x12AB9C));
    assert_eq!(t.status.as_ref(), "Page color: #12AB9C");
    // It opens on the current colour.
    open(&mut t, more_colors_dialog);
    assert_eq!(control(&t, "hex").text(), "12AB9C");
}

#[test]
fn fill_effects_writes_a_gradient_and_round_trips() {
    let mut t = three_sections();
    open(&mut t, fill_effects_dialog);
    assert!(!control(&t, "color2").enabled, "one colour by default");
    set(&mut t, "colors", s("Two colors"));
    assert!(control(&t, "color2").enabled);
    set(&mut t, "color1", s("Light Blue"));
    set(&mut t, "color2", s("Purple"));
    set(&mut t, "style", s("Diagonal up"));
    click(&mut t, "OK").unwrap();
    let want = PageBackground {
        color: 0x00B0F0,
        gradient: Some(Gradient {
            color2: 0x7030A0,
            style: GradientStyle::DiagonalUp,
        }),
    };
    assert_eq!(page_background(&t), Some(want));
    assert_eq!(reopen(&t).page_background(), Some(want));
    // Reopening shows it.
    open(&mut t, fill_effects_dialog);
    assert_eq!(control(&t, "colors").text(), "Two colors");
    assert_eq!(control(&t, "color2").text(), "Purple");
    assert_eq!(control(&t, "style").text(), "Diagonal up");
    // One colour goes to white.
    set(&mut t, "colors", s("One color"));
    click(&mut t, "OK").unwrap();
    assert_eq!(
        page_background(&t)
            .and_then(|b| b.gradient)
            .map(|g| g.color2),
        Some(0xFFFFFF)
    );
}

#[test]
fn custom_watermark_writes_text_size_colour_and_layout_then_removes() {
    let mut t = three_sections();
    open(&mut t, watermark_dialog);
    assert_eq!(control(&t, "kind").text(), "No watermark");
    assert!(
        !control(&t, "text").enabled,
        "fields wait for Text watermark"
    );
    set(&mut t, "kind", s("Text watermark"));
    assert!(control(&t, "text").enabled);
    set(&mut t, "text", s("Internal & <draft>"));
    set(&mut t, "font", s("Georgia"));
    set(&mut t, "size", s("x"));
    let err = click(&mut t, "OK").unwrap_err();
    assert!(err.contains("Size"), "{err}");
    set(&mut t, "size", s("72"));
    set(&mut t, "color", s("Red"));
    set(&mut t, "semi", Json::Bool(false));
    set(&mut t, "layout", s("Horizontal"));
    click(&mut t, "OK").unwrap();
    assert!(t.dirty);
    let pkg = reopen(&t);
    let shown = pkg.shown_text_watermarks(&ed(&t).sections());
    assert_eq!(shown.len(), 1);
    let w = &shown[0];
    assert_eq!(w.text, "Internal & <draft>");
    assert_eq!(w.font_size_pt, Some(72.0));
    assert_eq!(w.fill, Some((0xFF, 0, 0)));
    assert_eq!(w.rotation, 0.0);
    let hdr: String = pkg
        .part_names()
        .iter()
        .filter(|n| n.starts_with("word/header"))
        .map(|n| pkg.part_text(n).unwrap())
        .collect();
    assert!(hdr.contains("font-family:&quot;Georgia&quot;"), "{hdr}");
    assert!(!hdr.contains("opacity"));

    // It reopens on the watermark; No watermark removes it.
    open(&mut t, watermark_dialog);
    assert_eq!(control(&t, "kind").text(), "Text watermark");
    assert_eq!(control(&t, "text").text(), "Internal & <draft>");
    assert_eq!(control(&t, "size").text(), "72");
    assert_eq!(control(&t, "layout").text(), "Horizontal");
    assert_eq!(control(&t, "font").text(), "Georgia");
    assert_eq!(control(&t, "semi").value, Value::Bool(false));
    assert_eq!(control(&t, "color").text(), "Red");
    // OK on it unchanged keeps the font and the opacity.
    click(&mut t, "OK").unwrap();
    let pkg = reopen(&t);
    let shown = pkg.shown_text_watermarks(&ed(&t).sections());
    assert_eq!(shown[0].font.as_deref(), Some("Georgia"));
    assert_eq!(shown[0].opacity, None);
    open(&mut t, watermark_dialog);
    set(&mut t, "kind", s("No watermark"));
    click(&mut t, "OK").unwrap();
    assert!(crate::design_tab::current_watermark(&t).is_none());
}

#[test]
fn custom_watermark_apply_keeps_the_dialog_open() {
    let mut t = three_sections();
    open(&mut t, watermark_dialog);
    set(&mut t, "kind", s("Text watermark"));
    set(&mut t, "text", s("REVIEW"));
    click(&mut t, "Apply").unwrap();
    assert_eq!(t.dialogs.top_id(), "watermark");
    assert_eq!(
        crate::design_tab::current_watermark(&t).map(|w| w.text),
        Some("REVIEW".into())
    );
}

/// Box on the whole document: every section, one undo step.
#[test]
fn page_borders_box_on_the_whole_document_is_one_undo_step() {
    let mut t = three_sections();
    let before = ed(&t).sections();
    open(&mut t, page_borders_dialog);
    assert_eq!(control(&t, "setting").text(), "None");
    assert!(!control(&t, "top").enabled, "sides are for Custom");
    set(&mut t, "setting", s("Box"));
    assert_eq!(control(&t, "top").value, Value::Bool(true));
    set(&mut t, "style", s("Double"));
    set(&mut t, "color", s("Blue"));
    set(&mut t, "width", s("1 1/2 pt"));
    click(&mut t, "OK").unwrap();
    assert!(t.dirty);
    for k in 0..3 {
        let pb = borders(&t, k).unwrap_or_else(|| panic!("section {k}"));
        assert_eq!(pb.display, PgBorderDisplay::AllPages);
        assert_eq!(pb.offset_from, PgBorderOffset::Page);
        for side in &pb.sides {
            let side = side.as_ref().unwrap();
            assert_eq!(
                (side.style.as_str(), side.sz, side.space, side.color),
                ("double", 12, 24, Some(0x0070C0))
            );
            assert!(!side.shadow && !side.frame);
        }
    }
    assert!(ed_mut(&mut t).undo());
    assert_eq!(ed(&t).sections(), before);
}

#[test]
fn shadow_and_three_d_flag_every_side() {
    for (setting, shadow, frame) in [("Shadow", true, false), ("3-D", false, true)] {
        let mut t = three_sections();
        open(&mut t, page_borders_dialog);
        set(&mut t, "setting", s(setting));
        click(&mut t, "OK").unwrap();
        let pb = borders(&t, 1).unwrap();
        assert!(pb.sides.iter().all(|s| {
            s.as_ref()
                .is_some_and(|s| s.shadow == shadow && s.frame == frame)
        }));
        // It reopens on the same setting.
        open(&mut t, page_borders_dialog);
        assert_eq!(control(&t, "setting").text(), setting);
    }
}

/// Custom with two sides, on the caret's section's first page only, with
/// Options... margins measured from the text.
#[test]
fn custom_sides_this_section_first_page_and_options() {
    let mut t = three_sections();
    open(&mut t, page_borders_dialog);
    set(&mut t, "setting", s("Custom"));
    assert!(control(&t, "top").enabled);
    set(&mut t, "top", Json::Bool(true));
    set(&mut t, "left", Json::Bool(false));
    set(&mut t, "bottom", Json::Bool(true));
    set(&mut t, "right", Json::Bool(false));
    set(&mut t, "apply", s("This section - First page only"));
    // Options... writes back into Page Borders, which stays open.
    click(&mut t, "Options...").unwrap();
    assert_eq!(t.dialogs.top_id(), "page-border-options");
    assert_eq!(control(&t, "top").text(), "24");
    assert_eq!(control(&t, "from").text(), "Edge of page");
    set(&mut t, "top", s("10"));
    set(&mut t, "from", s("Text"));
    click(&mut t, "OK").unwrap();
    assert_eq!(t.dialogs.top_id(), "page-borders");
    assert_eq!(borders(&t, 1), None, "nothing applied yet");
    click(&mut t, "OK").unwrap();
    assert_eq!(borders(&t, 0), None, "only the caret's section");
    assert_eq!(borders(&t, 2), None);
    let pb = borders(&t, 1).unwrap();
    assert_eq!(pb.display, PgBorderDisplay::FirstPage);
    assert_eq!(pb.offset_from, PgBorderOffset::Text);
    let spaces: Vec<Option<u32>> = pb
        .sides
        .iter()
        .map(|s| s.as_ref().map(|s| s.space))
        .collect();
    assert_eq!(spaces, [Some(10), None, Some(24), None]);
    let raw = &ed(&t).sections()[1];
    assert!(
        raw.contains("<w:top w:val=\"single\" w:sz=\"4\" w:space=\"10\""),
        "{raw}"
    );

    // Reopened, it shows Custom with those sides and the margins.
    open(&mut t, page_borders_dialog);
    assert_eq!(control(&t, "setting").text(), "Custom");
    assert_eq!(control(&t, "left").value, Value::Bool(false));
    assert_eq!(
        control(&t, "apply").text(),
        "This section - First page only"
    );
    click(&mut t, "Options...").unwrap();
    assert_eq!(control(&t, "top").text(), "10");
    assert_eq!(control(&t, "from").text(), "Text");
    click(&mut t, "Cancel").unwrap();
    click(&mut t, "Cancel").unwrap();
    assert!(!t.dialogs.is_open());
}

/// Options... margins that differ per side leave Box a Box.
#[test]
fn box_with_one_different_margin_reopens_as_box() {
    let mut t = three_sections();
    open(&mut t, page_borders_dialog);
    set(&mut t, "setting", s("Box"));
    click(&mut t, "Options...").unwrap();
    set(&mut t, "top", s("10"));
    click(&mut t, "OK").unwrap();
    click(&mut t, "OK").unwrap();
    open(&mut t, page_borders_dialog);
    assert_eq!(control(&t, "setting").text(), "Box");
    assert!(!control(&t, "top").enabled);
}

#[test]
fn all_except_first_page_and_a_bad_margin() {
    let mut t = three_sections();
    open(&mut t, page_borders_dialog);
    set(&mut t, "setting", s("Box"));
    set(&mut t, "apply", s("This section - All except first page"));
    click(&mut t, "Options...").unwrap();
    set(&mut t, "right", s("40"));
    click(&mut t, "OK").unwrap();
    let err = click(&mut t, "OK").unwrap_err();
    assert!(err.contains("Right margin"), "{err}");
    assert_eq!(t.dialogs.top_id(), "page-borders", "stays open to fix");
    click(&mut t, "Options...").unwrap();
    set(&mut t, "right", s("31"));
    click(&mut t, "OK").unwrap();
    click(&mut t, "OK").unwrap();
    assert_eq!(
        borders(&t, 1).map(|p| p.display),
        Some(PgBorderDisplay::NotFirstPage)
    );
}

/// Setting None removes `w:pgBorders` entirely, and the borders survive
/// save until then.
#[test]
fn setting_none_removes_page_borders() {
    let mut t = three_sections();
    open(&mut t, page_borders_dialog);
    set(&mut t, "setting", s("Box"));
    click(&mut t, "OK").unwrap();
    let pkg = reopen(&t);
    assert!(pkg.has_page_borders());
    open(&mut t, page_borders_dialog);
    assert_eq!(control(&t, "setting").text(), "Box");
    set(&mut t, "setting", s("None"));
    assert_eq!(control(&t, "top").value, Value::Bool(false));
    click(&mut t, "OK").unwrap();
    assert!(ed(&t).sections().iter().all(|s| !s.contains("w:pgBorders")));
    assert_eq!(t.status.as_ref(), "Page borders removed");
}

/// Sides that differ, and a width not in the list, survive an OK that
/// changed nothing; changing Color recolours every side but keeps the rest.
#[test]
fn ok_on_an_untouched_dialog_keeps_each_sides_own_look() {
    let mut t = three_sections();
    let side = |style: &str, sz, color| {
        Some(BorderSide {
            style: style.into(),
            sz,
            space: 4,
            color,
            shadow: false,
            frame: false,
        })
    };
    let pb = PageBorders {
        sides: [
            side("double", 10, Some(0xFF0000)),
            None,
            side("dotted", 4, None),
            None,
        ],
        ..Default::default()
    };
    ed_mut(&mut t).edit_sections(&[1], |raw| PageBorders::apply(Some(&pb), raw));
    t.dirty = false;
    let before = ed(&t).sections();
    open(&mut t, page_borders_dialog);
    assert_eq!(control(&t, "setting").text(), "Custom");
    assert_eq!(
        control(&t, "apply").text(),
        "This section",
        "only section 2 has them"
    );
    click(&mut t, "OK").unwrap();
    assert_eq!(ed(&t).sections(), before, "byte for byte");
    assert!(!t.dirty);

    open(&mut t, page_borders_dialog);
    set(&mut t, "color", s("Blue"));
    click(&mut t, "OK").unwrap();
    let got = borders(&t, 1).unwrap();
    let top = got.sides[0].as_ref().unwrap();
    let bottom = got.sides[2].as_ref().unwrap();
    assert_eq!(
        (top.style.as_str(), top.sz, top.color),
        ("double", 10, Some(0x0070C0))
    );
    assert_eq!(
        (bottom.style.as_str(), bottom.sz, bottom.color),
        ("dotted", 4, Some(0x0070C0))
    );
    assert_eq!(borders(&t, 0), None, "This section only");
}

/// An Art border (not in the style list) opens as itself.
#[test]
fn an_art_border_prefills_without_losing_its_name() {
    let mut t = three_sections();
    ed_mut(&mut t).edit_sections(&[1], |raw| {
        let pb = PageBorders {
            sides: std::array::from_fn(|_| {
                Some(BorderSide {
                    style: "apples".into(),
                    sz: 20,
                    space: 4,
                    color: None,
                    shadow: false,
                    frame: false,
                })
            }),
            ..Default::default()
        };
        PageBorders::apply(Some(&pb), raw)
    });
    open(&mut t, page_borders_dialog);
    assert_eq!(control(&t, "style").text(), "apples");
    assert_eq!(control(&t, "setting").text(), "Box");
}

/// Page Borders works on a document without a package, not on Markdown.
#[test]
fn page_borders_need_a_docx_not_a_package() {
    let mut t = three_sections();
    t.pkg = None;
    open(&mut t, page_borders_dialog);
    set(&mut t, "setting", s("Box"));
    click(&mut t, "OK").unwrap();
    assert!(borders(&t, 0).is_some());
    t.markdown = true;
    let err = page_borders_dialog(&t).unwrap_err();
    assert!(err.contains(".docx"), "{err}");
}
