//! A pasted cell's format saved into another workbook (#1011): the source
//! entry's raw parts read in one pass ([`foreign_xfs`]) and appended by
//! [`splice_styles`] where the destination has no equal element.

use super::*;
use crate::edit::ClipBlock;
use crate::sheet::Cell;

/// An underlined theme-coloured font, a theme fill and a double border.
const FONT: &str = "<font><u/><sz val=\"11\"/><color theme=\"1\"/><name val=\"Calibri\"/><family val=\"2\"/><scheme val=\"minor\"/></font>";
const FILL: &str = "<fill><patternFill patternType=\"solid\"><fgColor theme=\"4\"/><bgColor indexed=\"64\"/></patternFill></fill>";
const BORDER: &str = "<border><left style=\"double\"/><right style=\"double\"/><top/><bottom style=\"double\"/><diagonal/></border>";

/// A fresh workbook whose styles part gains `FONT`, `FILL` and `BORDER` and
/// two `<cellXfs>` entries: `0.00%` (built-in 10) in all three with a top
/// vertical alignment and unlocked, and General in `FONT` alone. Saved and
/// loaded again, with the two entries' indices.
fn source() -> (SheetPackage, u32, u32) {
    let mut pkg = new_xlsx();
    let xml = String::from_utf8(pkg.part("xl/styles.xml").unwrap().to_vec()).unwrap();
    let (nf, nl, nb) = (
        read_count(&xml, "<fonts"),
        read_count(&xml, "<fills"),
        read_count(&xml, "<borders"),
    );
    let mut xml = bump_count(&xml, "<fonts", 1);
    xml = xml.replacen("</fonts>", &format!("{FONT}</fonts>"), 1);
    xml = bump_count(&xml, "<fills", 1);
    xml = xml.replacen("</fills>", &format!("{FILL}</fills>"), 1);
    xml = bump_count(&xml, "<borders", 1);
    xml = xml.replacen("</borders>", &format!("{BORDER}</borders>"), 1);
    xml = bump_count(&xml, "<cellXfs", 2);
    xml = xml.replacen(
        "</cellXfs>",
        &format!(
            "<xf numFmtId=\"10\" fontId=\"{nf}\" fillId=\"{nl}\" borderId=\"{nb}\" xfId=\"0\" applyNumberFormat=\"1\" applyFont=\"1\" applyFill=\"1\" applyBorder=\"1\" applyAlignment=\"1\" applyProtection=\"1\"><alignment vertical=\"top\"/><protection locked=\"0\"/></xf><xf numFmtId=\"0\" fontId=\"{nf}\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/></cellXfs>"
        ),
        1,
    );
    pkg.set_part("xl/styles.xml", xml.into_bytes());
    let pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
    let n = pkg.workbook.styles.xfs.len() as u32;
    (pkg, n - 2, n - 1)
}

fn styles_xml(pkg: &SheetPackage) -> String {
    String::from_utf8(pkg.part("xl/styles.xml").unwrap().to_vec()).unwrap()
}

#[test]
fn foreign_xfs_reads_the_raw_parts() {
    let (src, pct, plain) = source();
    let found = src.foreign_xfs([pct, plain, 999]);
    assert_eq!(
        found.keys().copied().collect::<Vec<_>>(),
        [pct, plain],
        "no entry 999"
    );
    let f = &found[&pct];
    assert_eq!(f.font.as_deref(), Some(FONT));
    assert_eq!(f.fill.as_deref(), Some(FILL));
    assert_eq!(f.border.as_deref(), Some(BORDER));
    assert_eq!(f.numfmt_id, Some(10));
    assert_eq!(
        f.alignment.as_deref(),
        Some("<alignment vertical=\"top\"/>")
    );
    assert_eq!(f.protection.as_deref(), Some("<protection locked=\"0\"/>"));
    assert_eq!(f.parsed.code.as_deref(), Some("0.00%"));
    assert!(f.parsed.unlocked);
    assert_eq!(
        (f.parsed.loaded_from, f.parsed.foreign.is_none()),
        (None, true)
    );
    assert_eq!(found[&plain].alignment, None);
    assert_eq!(found[&plain].numfmt_id, Some(0));

    // A prefixed part's elements would mean nothing in another part.
    let prefixed_part = concat!(
        "<x:styleSheet xmlns:x=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
        "<x:fonts count=\"1\"><x:font><x:sz val=\"11\"/></x:font></x:fonts>",
        "<x:cellXfs count=\"1\"><x:xf numFmtId=\"0\" fontId=\"0\"/></x:cellXfs>",
        "</x:styleSheet>"
    );
    assert!(foreign_xfs(prefixed_part, [0]).is_empty());
    // A built-in id the part redefines is not the built-in.
    let redefined = concat!(
        "<styleSheet><numFmts count=\"1\"><numFmt numFmtId=\"10\" formatCode=\"0.0%\"/></numFmts>",
        "<fonts count=\"1\"><font><sz val=\"11\"/></font></fonts>",
        "<cellXfs count=\"1\"><xf numFmtId=\"10\" fontId=\"0\"/></cellXfs></styleSheet>"
    );
    assert_eq!(foreign_xfs(redefined, [0])[&0].numfmt_id, None);
}

/// A copy of `rows` × column A of `src`'s first sheet, carrying its raw
/// parts, pasted into `dst`'s first sheet at the same rows.
fn paste_across(src: &SheetPackage, dst: &mut SheetPackage, rows: Vec<u32>) {
    let mut block = ClipBlock::capture(&src.workbook, 0, rows.clone(), vec![0]);
    block.carry_parts(|ids| src.foreign_xfs(ids));
    block.adopt_styles(&mut dst.workbook.styles);
    for (row, r) in block.cells.iter().zip(rows) {
        dst.workbook.sheets[0].set_cell(r, 0, row[0].clone());
    }
}

/// The destination: a fresh workbook with an italic xf at the source's
/// percent entry's index, so a raw id would land on it.
fn destination(at: u32) -> SheetPackage {
    let mut pkg = new_xlsx();
    let mut xml = styles_xml(&pkg);
    let nf = read_count(&xml, "<fonts");
    xml = bump_count(&xml, "<fonts", 1);
    xml = xml.replacen(
        "</fonts>",
        "<font><i/><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>",
        1,
    );
    let have = cell_xf_elements(&xml).len() as u32;
    let pad = at + 1 - have;
    xml = bump_count(&xml, "<cellXfs", pad);
    let xf = format!(
        "<xf numFmtId=\"0\" fontId=\"{nf}\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/>"
    );
    xml = xml.replacen(
        "</cellXfs>",
        &format!("{}</cellXfs>", xf.repeat(pad as usize)),
        1,
    );
    pkg.set_part("xl/styles.xml", xml.into_bytes());
    load_xlsx(&save_xlsx(&pkg)).unwrap()
}

#[test]
fn splice_styles_appends_a_foreign_xfs_own_parts() {
    let (src, pct, _) = source();
    let mut src = src;
    for r in 0..2 {
        src.workbook.sheets[0].set_cell(
            r,
            0,
            Cell {
                style: pct,
                ..Cell::number(0.5)
            },
        );
    }
    let mut dst = destination(pct);
    assert!(dst.workbook.styles.xf(pct).italic);
    let orig = styles_xml(&dst);
    paste_across(&src, &mut dst, vec![0, 1]);
    // A format command on the second pasted cell: bold.
    let mut bold = dst
        .workbook
        .styles
        .xf(dst.workbook.sheets[0].cell(1, 0).unwrap().style);
    bold.bold = true;
    let b = dst.workbook.styles.intern(bold);
    dst.workbook.sheets[0].cells.get_mut(&(1, 0)).unwrap().style = b;

    let saved = load_xlsx(&save_xlsx(&dst)).unwrap();
    let xml = styles_xml(&saved);
    let xfs = cell_xf_elements(&xml);
    let was = cell_xf_elements(&orig);
    assert_eq!(
        xfs[..was.len()],
        was[..],
        "the destination's own entries are unchanged"
    );
    let fonts = font_elements(&xml);
    let fills = section_children(&xml, "fills", "fill");
    let borders = section_children(&xml, "borders", "border");
    let style_of = |r: u32| saved.workbook.sheets[0].cell(r, 0).unwrap().style as usize;
    let attr = |i: usize, a: &str| -> usize { tag_attr(xfs[i], a).unwrap().parse().unwrap() };

    let i = style_of(0);
    assert!(i >= was.len(), "a new entry: {}", xfs[i]);
    assert_eq!(fonts[attr(i, "fontId")], FONT);
    assert_eq!(fills[attr(i, "fillId")], FILL);
    assert_eq!(borders[attr(i, "borderId")], BORDER);
    assert_eq!(attr(i, "numFmtId"), 10);
    assert_eq!(attr(i, "xfId"), 0);
    assert!(
        xfs[i].contains("<alignment vertical=\"top\"/>"),
        "{}",
        xfs[i]
    );
    assert!(xfs[i].contains("<protection locked=\"0\"/>"), "{}", xfs[i]);
    let xf = saved.workbook.styles.xf(i as u32);
    assert_eq!(xf.code.as_deref(), Some("0.00%"));
    assert!(xf.unlocked && !xf.italic);

    // The bolded cell: the source font with only `<b/>` added, the rest kept.
    let j = style_of(1);
    assert_eq!(
        fonts[attr(j, "fontId")],
        FONT.replacen("<font>", "<font><b/>", 1)
    );
    assert_eq!(borders[attr(j, "borderId")], BORDER);
    assert_eq!(attr(j, "fillId"), attr(i, "fillId"), "one fill for both");
    assert!(xfs[j].contains("<protection locked=\"0\"/>"));
    assert!(saved.workbook.styles.xf(j as u32).bold);

    // The destination's own xf at the source's index is still italic.
    assert!(saved.workbook.styles.xf(pct).italic);
}

#[test]
fn splice_styles_dedupes_appended_parts() {
    let (mut src, pct, plain) = source();
    for (r, s) in [pct, plain].into_iter().enumerate() {
        src.workbook.sheets[0].set_cell(
            r as u32,
            0,
            Cell {
                style: s,
                ..Cell::number(1.0)
            },
        );
    }
    let mut dst = new_xlsx();
    let orig = styles_xml(&dst);
    paste_across(&src, &mut dst, vec![0, 1]);
    let saved = save_xlsx(&dst);
    let xml = styles_xml(&load_xlsx(&saved).unwrap());
    // The two entries share one font, appended once; the plain entry's
    // fill and border are the destination's own first ones, appended never.
    assert_eq!(read_count(&xml, "<fonts"), read_count(&orig, "<fonts") + 1);
    assert_eq!(read_count(&xml, "<fills"), read_count(&orig, "<fills") + 1);
    assert_eq!(
        read_count(&xml, "<borders"),
        read_count(&orig, "<borders") + 1
    );
    let xfs = cell_xf_elements(&xml);
    let last = xfs[xfs.len() - 1];
    assert_eq!(tag_attr(last, "fillId"), Some("0"), "{last}");
    assert_eq!(tag_attr(last, "borderId"), Some("0"), "{last}");
    assert_eq!(
        tag_attr(last, "fontId"),
        tag_attr(xfs[xfs.len() - 2], "fontId")
    );
}

/// #1011 r1: a destination that redefines the built-in id a pasted xf uses
/// (10, `0.00%`, as `0.0%`) gets the source's code as a custom format.
#[test]
fn a_built_in_id_the_destination_redefines_is_minted() {
    let (mut src, pct, _) = source();
    src.workbook.sheets[0].set_cell(
        0,
        0,
        Cell {
            style: pct,
            ..Cell::number(0.5)
        },
    );
    let mut dst = new_xlsx();
    let xml = styles_xml(&dst).replacen(
        "<fonts",
        "<numFmts count=\"1\"><numFmt numFmtId=\"10\" formatCode=\"0.0%\"/></numFmts><fonts",
        1,
    );
    dst.set_part("xl/styles.xml", xml.into_bytes());
    let mut dst = load_xlsx(&save_xlsx(&dst)).unwrap();
    paste_across(&src, &mut dst, vec![0]);
    let saved = load_xlsx(&save_xlsx(&dst)).unwrap();
    let xml = styles_xml(&saved);
    let s = saved.workbook.sheets[0].cell(0, 0).unwrap().style;
    let pasted = cell_xf_elements(&xml)[s as usize];
    let id: u32 = tag_attr(pasted, "numFmtId").unwrap().parse().unwrap();
    assert!(id >= 164, "{pasted}");
    assert_eq!(saved.workbook.styles.xf(s).code.as_deref(), Some("0.00%"));
}
