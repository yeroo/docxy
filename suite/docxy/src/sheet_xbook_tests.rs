//! A copy pasted into another workbook (#1011): its cells land with their
//! own formats, adopted into the destination's styles, not with whatever
//! the destination's xfs hold at the source's style ids, and a save keeps
//! what the model doesn't carry of them (borders, underline, unlocked).
//! Ctrl+V and a cut pasted as a copy (`paste_grid_clip`), Paste Special and
//! the gallery (`paste_item_at`), Paste Options (`repaste`).

use super::*;
use crate::sheet_menus::PasteItem;
use crate::sheet_paste::PasteOptions;
use core::prelude::v1::test;
use gridcore::edit::{PasteSpec, PasteWhat};
use gridcore::sheet::{Cell, Xf, parse_cell_name};

fn view() -> SheetView {
    let Surface::Sheet(v) = new_sheet_surface() else {
        panic!("a new sheet surface")
    };
    v
}

fn at(name: &str) -> (u32, u32) {
    parse_cell_name(name).unwrap()
}

fn put(v: &mut SheetView, name: &str, cell: Cell) {
    let (r, c) = at(name);
    let s = v.active;
    v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
}

fn select(v: &mut SheetView, name: &str) {
    v.anchor = at(name);
    v.sel = at(name);
    v.clear_areas();
}

fn copy(v: &mut SheetView, from: &str, to: &str, cut: bool) -> GridClip {
    v.anchor = at(from);
    v.sel = at(to);
    v.clear_areas();
    v.grid_clip(cut).expect("something to copy")
}

fn style(v: &SheetView, name: &str) -> u32 {
    let (r, c) = at(name);
    v.sheet().cell(r, c).map_or(0, |c| c.style)
}

fn xf(v: &SheetView, name: &str) -> Xf {
    v.pkg.workbook.styles.xf(style(v, name))
}

/// Bold, a red fill and `0.00%`.
fn percent() -> Xf {
    let mut x = Xf {
        bold: true,
        fill: Some((255, 0, 0)),
        ..Xf::default()
    };
    x.set_code(Some("0.00%".into()));
    x
}

/// The format alone, whichever part it came from.
fn modeled(x: Xf) -> Xf {
    Xf {
        loaded_from: None,
        foreign: None,
        ..x
    }
}

/// Source A: A1 in [`percent`], A2 plain. Destination B: an italic xf at
/// the index A's percent xf has, so a raw id lands on it. Both fresh
/// workbooks, so their own xfs line up.
fn books() -> (SheetView, SheetView, u32) {
    let mut a = view();
    let mut b = view();
    let k = a.pkg.workbook.styles.intern(percent());
    let italic = b.pkg.workbook.styles.intern(Xf {
        italic: true,
        ..Xf::default()
    });
    assert_eq!(k, italic, "the same index in both");
    put(
        &mut a,
        "A1",
        Cell {
            style: k,
            ..Cell::number(0.25)
        },
    );
    put(&mut a, "A2", Cell::number(2.0));
    (a, b, k)
}

/// B's own xfs are unchanged, and only `added` were appended to them.
fn appended(b: &SheetView, before: &[Xf], added: usize) {
    let xfs = &b.pkg.workbook.styles.xfs;
    assert_eq!(xfs[..before.len()], before[..], "B's own xfs unchanged");
    assert_eq!(xfs.len(), before.len() + added, "{added} appended");
}

#[test]
fn a_paste_from_another_workbook_keeps_its_formats() {
    let (mut a, mut b, k) = books();
    let before = b.pkg.workbook.styles.xfs.clone();
    let clip = copy(&mut a, "A1", "A2", false);
    select(&mut b, "C1");
    assert_eq!(b.paste_grid_clip(&clip), Ok(GridPasted::Done));
    assert_ne!(style(&b, "C1"), k, "not B's italic xf");
    assert_eq!(modeled(xf(&b, "C1")), percent());
    assert_eq!(style(&b, "C2"), 0, "a plain cell stays on B's default");
    appended(&b, &before, 1);

    // Pasted again: the xf the first paste appended.
    select(&mut b, "E1");
    assert_eq!(b.paste_grid_clip(&clip), Ok(GridPasted::Done));
    assert_eq!(style(&b, "E1"), style(&b, "C1"));
    appended(&b, &before, 1);

    // A cut from A lands in B as a copy, in its own format too.
    let cut = copy(&mut a, "A1", "A1", true);
    select(&mut b, "G1");
    assert!(matches!(
        b.paste_grid_clip(&cut),
        Ok(GridPasted::KeptAsCopy(_))
    ));
    assert_eq!(style(&b, "G1"), style(&b, "C1"));
    appended(&b, &before, 1);

    // Within A, a paste keeps A's ids and appends nothing.
    let n = a.pkg.workbook.styles.xfs.len();
    let own = copy(&mut a, "A1", "A2", false);
    select(&mut a, "C1");
    assert_eq!(a.paste_grid_clip(&own), Ok(GridPasted::Done));
    assert_eq!((style(&a, "C1"), a.pkg.workbook.styles.xfs.len()), (k, n));
}

#[test]
fn paste_special_from_another_workbook_keeps_its_formats() {
    let (mut a, mut b, k) = books();
    let before = b.pkg.workbook.styles.xfs.clone();
    let clip = copy(&mut a, "A1", "A2", false);
    // Paste Special › Formats, the dialog's way.
    b.paste_special_at(
        &clip.block,
        &PasteSpec::of(PasteWhat::Formats),
        at("C1"),
        true,
    )
    .unwrap();
    assert_eq!(modeled(xf(&b, "C1")), percent());
    appended(&b, &before, 1);
    // Paste Special › All.
    b.paste_special_at(&clip.block, &PasteSpec::of(PasteWhat::All), at("E1"), true)
        .unwrap();
    assert_eq!(style(&b, "E1"), style(&b, "C1"));
    assert_ne!(style(&b, "E1"), k);
    // A gallery item: Keep Source Formatting.
    b.paste_item_at(&clip.block, PasteItem::KeepSourceFormatting, at("G1"), true)
        .unwrap();
    assert_eq!(style(&b, "G1"), style(&b, "C1"));
    assert_eq!(style(&b, "G2"), 0);
    appended(&b, &before, 1);
}

#[test]
fn paste_options_repaste_from_another_workbook_keeps_its_formats() {
    let (mut a, mut b, _) = books();
    let before = b.pkg.workbook.styles.xfs.clone();
    let clip = copy(&mut a, "A1", "A1", false);
    select(&mut b, "C1");
    assert_eq!(b.paste_grid_clip(&clip), Ok(GridPasted::Done));
    let first = style(&b, "C1");
    // The button keeps the copy as it was taken, in A's ids.
    let opts = PasteOptions {
        view: b.id,
        edit_gen: b.edit_gen,
        block: clip.block.clone(),
        at: at("C1"),
        rect: b.range(),
        item: PasteItem::Paste,
        foreign: true,
        gone: Default::default(),
    };
    b.repaste(&opts, PasteItem::Formatting).unwrap();
    assert_eq!(style(&b, "C1"), first);
    assert_eq!(modeled(xf(&b, "C1")), percent());
    appended(&b, &before, 1);
}

#[test]
fn a_refused_foreign_paste_leaves_the_styles() {
    let (mut a, mut b, _) = books();
    let s = b.active;
    let mut d1 = Cell::formula("A1:A3*2");
    d1.f_attrs = Some(" t=\"array\" ref=\"D1:D3\"".into());
    b.pkg.workbook.sheets[s].set_cell(0, 3, d1);
    b.rebuild_engine();
    b.engine.recalc_all(&mut b.pkg.workbook);
    let before = b.pkg.workbook.styles.xfs.clone();
    let clip = copy(&mut a, "A1", "A1", false);
    // D2 is part of the array anchored at D1.
    let refused = b.paste_special_at(&clip.block, &PasteSpec::default(), at("D2"), true);
    assert_eq!(refused, Err(gridcore::engine::PART_OF_ARRAY.to_string()));
    assert_eq!(b.pkg.workbook.styles.xfs, before);
    select(&mut b, "D2");
    assert!(matches!(
        b.paste_grid_clip(&clip),
        Err(GridPasteError::Refused(_))
    ));
    assert_eq!(b.pkg.workbook.styles.xfs, before);
}

/// The styles part of a fresh workbook with one more `<cellXfs>` entry: an
/// underlined font, a dashed box border, a top vertical alignment and
/// unlocked, the parts the model doesn't carry.
const FONT: &str = "<font><u/><sz val=\"11\"/><name val=\"Calibri\"/></font>";
const BORDER: &str = "<border><left style=\"dashed\"/><right style=\"dashed\"/><top style=\"dashed\"/><bottom style=\"dashed\"/><diagonal/></border>";

fn bordered_source() -> (SheetView, u32) {
    let mut v = view();
    let name = "xl/styles.xml";
    let xml = String::from_utf8(v.pkg.part(name).unwrap().to_vec()).unwrap();
    let xml = xml
        .replacen("<fonts count=\"1\">", "<fonts count=\"2\">", 1)
        .replacen("</fonts>", &format!("{FONT}</fonts>"), 1)
        .replacen("<borders count=\"1\">", "<borders count=\"2\">", 1)
        .replacen("</borders>", &format!("{BORDER}</borders>"), 1)
        .replacen("<cellXfs count=\"1\">", "<cellXfs count=\"2\">", 1)
        .replacen(
            "</cellXfs>",
            "<xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"1\" xfId=\"0\" applyFont=\"1\" applyBorder=\"1\" applyAlignment=\"1\" applyProtection=\"1\"><alignment vertical=\"top\"/><protection locked=\"0\"/></xf></cellXfs>",
            1,
        );
    assert!(xml.contains("<cellXfs count=\"2\">"), "{xml}");
    v.pkg.set_part(name, xml.into_bytes());
    v.pkg = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&v.pkg)).unwrap();
    v.rebuild_engine();
    put(
        &mut v,
        "A1",
        Cell {
            style: 1,
            ..Cell::text("a")
        },
    );
    put(
        &mut v,
        "A2",
        Cell {
            style: 1,
            ..Cell::text("b")
        },
    );
    (v, 1)
}

fn styles_text(pkg: &gridcore::xlsx::SheetPackage) -> String {
    String::from_utf8(pkg.part("xl/styles.xml").unwrap().to_vec()).unwrap()
}

/// The `n`th raw `<child>` of `<section>`, found by plain text: the parts
/// here are unprefixed and comment-free.
fn nth<'a>(xml: &'a str, section: &str, child: &str, n: usize) -> &'a str {
    let s = xml.find(&format!("<{section}")).unwrap();
    let e = xml.find(&format!("</{section}>")).unwrap();
    let body = &xml[s..e];
    let body = &body[body.find('>').unwrap() + 1..];
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(&format!("<{child}")) {
        let tail = &rest[i..];
        let open_end = tail.find('>').unwrap() + 1;
        let end = if tail[..open_end].ends_with("/>") {
            open_end
        } else {
            let close = format!("</{child}>");
            tail.find(&close).unwrap() + close.len()
        };
        out.push(&tail[..end]);
        rest = &tail[end..];
    }
    out[n]
}

fn attr(tag: &str, name: &str) -> usize {
    let key = format!(" {name}=\"");
    let s = tag.find(&key).unwrap() + key.len();
    tag[s..s + tag[s..].find('"').unwrap()].parse().unwrap()
}

#[test]
fn a_cross_workbook_paste_saves_the_sources_borders() {
    let (mut a, k) = bordered_source();
    let mut b = view();
    // B's own xf at A's index: italic.
    let italic = b.pkg.workbook.styles.intern(Xf {
        italic: true,
        ..Xf::default()
    });
    assert_eq!(italic, k);
    let b_saved = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&b.pkg)).unwrap();
    b.pkg = b_saved;
    b.rebuild_engine();
    let orig = styles_text(&b.pkg);
    let own = (0..=k as usize)
        .map(|i| nth(&orig, "cellXfs", "xf", i).to_string())
        .collect::<Vec<_>>();

    let clip = copy(&mut a, "A1", "A2", false);
    select(&mut b, "C1");
    assert_eq!(b.paste_grid_clip(&clip), Ok(GridPasted::Done));
    // A format command on one pasted cell: bold.
    let mut bold = xf(&b, "C2");
    bold.bold = true;
    let bs = b.pkg.workbook.styles.intern(bold);
    put(
        &mut b,
        "C2",
        Cell {
            style: bs,
            ..Cell::text("b")
        },
    );

    let saved = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&b.pkg)).unwrap();
    let xml = styles_text(&saved);
    for (i, was) in own.iter().enumerate() {
        assert_eq!(nth(&xml, "cellXfs", "xf", i), was, "B's own entry {i}");
    }
    let style_at = |name: &str| {
        let (r, c) = at(name);
        saved.workbook.sheets[0].cell(r, c).unwrap().style as usize
    };
    let pasted = nth(&xml, "cellXfs", "xf", style_at("C1"));
    assert_eq!(nth(&xml, "fonts", "font", attr(pasted, "fontId")), FONT);
    assert_eq!(
        nth(&xml, "borders", "border", attr(pasted, "borderId")),
        BORDER
    );
    assert!(pasted.contains("<alignment vertical=\"top\"/>"), "{pasted}");
    assert!(pasted.contains("<protection locked=\"0\"/>"), "{pasted}");
    assert!(saved.workbook.styles.xf(style_at("C1") as u32).unlocked);
    // The bolded cell: the source's font with `<b/>`, its border kept.
    let bolded = nth(&xml, "cellXfs", "xf", style_at("C2"));
    assert_eq!(
        nth(&xml, "fonts", "font", attr(bolded, "fontId")),
        FONT.replacen("<font>", "<font><b/>", 1)
    );
    assert_eq!(
        nth(&xml, "borders", "border", attr(bolded, "borderId")),
        BORDER
    );
    assert!(bolded.contains("<protection locked=\"0\"/>"), "{bolded}");
    // B's own xf at A's index is still italic.
    assert!(saved.workbook.styles.xf(k).italic);
}
