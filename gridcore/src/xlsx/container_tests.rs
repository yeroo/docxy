//! How the ZIP container's entries become parts (#1108): separators other
//! writers use, and empty entries.

use super::*;
use crate::sheet::CellValue;
use opccore::zipwrite::write_zip;

const SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const THEME: &str = "<?xml version=\"1.0\"?><a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"Office\"/>";
const PKG_RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// A one-sheet workbook laid out as LibreOffice's tdf76115.xlsx is: the
/// sheet at `xl/sheet1.xml`, every relationship target absolute, and each
/// entry name written with `sep` between its segments. It also has a theme,
/// a part repair can drop.
fn package(sep: &str) -> Vec<(String, Vec<u8>)> {
    let ct = "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/></Types>";
    let root = format!(
        "<?xml version=\"1.0\"?><Relationships xmlns=\"{PKG_RELS}\"><Relationship Id=\"rId1\" Type = \"{R}/officeDocument\" Target=\"/xl/workbook.xml\"/></Relationships>"
    );
    let wb = format!(
        "<?xml version=\"1.0\"?><workbook xmlns=\"{SML}\" xmlns:r=\"{R}\"><sheets><sheet name=\"Plan1\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
    );
    let wb_rels = format!(
        "<?xml version=\"1.0\"?><Relationships xmlns=\"{PKG_RELS}\"><Relationship Id=\"rId1\" Type=\"{R}/worksheet\" Target=\"/xl/sheet1.xml\"/><Relationship Id=\"rId2\" Type=\"{R}/theme\" Target=\"/xl/theme/theme1.xml\"/></Relationships>"
    );
    let sheet = format!(
        "<?xml version=\"1.0\"?><worksheet xmlns=\"{SML}\"><sheetData><row r=\"1\"><c r=\"A1\"><v>42</v></c></row></sheetData></worksheet>"
    );
    [
        ("[Content_Types].xml", ct.to_string()),
        ("_rels/.rels", root),
        ("xl/_rels/workbook.xml.rels", wb_rels),
        ("xl/workbook.xml", wb),
        ("xl/sheet1.xml", sheet),
        ("xl/theme/theme1.xml", THEME.to_string()),
    ]
    .into_iter()
    .map(|(name, xml)| (name.replace('/', sep), xml.into_bytes()))
    .collect()
}

fn a1(pkg: &SheetPackage) -> &CellValue {
    &pkg.workbook.sheets[0].cells[&(0, 0)].value
}

/// #1095: `xl\workbook.xml` names the part `xl/workbook.xml`; the workbook,
/// its relationships and its sheet are found, and a save writes OPC names.
#[test]
fn backslash_part_names_load_and_save_with_slashes() {
    let pkg = load_xlsx(&write_zip(&package("\\"))).expect("loads");
    assert_eq!(a1(&pkg), &CellValue::Number(42.0));
    assert_eq!(pkg.workbook.sheets[0].name, "Plan1");
    for name in pkg.part_names() {
        assert!(!name.contains('\\'), "{name}");
    }
    let saved = save_xlsx(&pkg);
    let zip = ZipArchive::open(&saved).unwrap();
    let names: Vec<&str> = zip.entries().iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"xl/workbook.xml"), "{names:?}");
    assert!(names.contains(&"xl/sheet1.xml"), "{names:?}");
    assert!(names.iter().all(|n| !n.contains('\\')), "{names:?}");
    let back = load_xlsx(&saved).expect("the save reloads");
    assert_eq!(a1(&back), &CellValue::Number(42.0));
}

/// Two entries that differ only in their separators would be one part.
#[test]
fn entries_that_become_one_part_are_corrupt() {
    let mut parts = package("/");
    parts.push(("xl\\sheet1.xml".into(), b"<worksheet/>".to_vec()));
    assert_eq!(
        load_xlsx(&write_zip(&parts)).err(),
        Some(XlsxError::CorruptPart)
    );
    assert_eq!(
        load_xlsx_repair(&write_zip(&parts)).err(),
        Some(XlsxError::CorruptPart)
    );
    // The same name twice is not a separator collision: it loads as before.
    let mut parts = package("/");
    parts.push(("xl/extra.bin".into(), b"1".to_vec()));
    parts.push(("xl/extra.bin".into(), b"2".to_vec()));
    assert!(load_xlsx(&write_zip(&parts)).is_ok());
}

/// `zip` with the entry `name` switched to the deflate method in both its
/// headers. An empty STORED entry becomes a deflate entry with no stream.
fn deflate_method(mut zip: Vec<u8>, name: &str) -> Vec<u8> {
    let (local, central) = {
        let arc = ZipArchive::open(&zip).unwrap();
        let local = arc.find(name).unwrap().local_offset as usize;
        let eocd = zip.len() - 22;
        let mut p = u32::from_le_bytes(zip[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        loop {
            let len = |at: usize| u16::from_le_bytes(zip[at..at + 2].try_into().unwrap()) as usize;
            let name_len = len(p + 28);
            if &zip[p + 46..p + 46 + name_len] == name.as_bytes() {
                break (local, p);
            }
            p += 46 + name_len + len(p + 30) + len(p + 32);
        }
    };
    zip[local + 8..local + 10].copy_from_slice(&8u16.to_le_bytes());
    zip[central + 10..central + 12].copy_from_slice(&8u16.to_le_bytes());
    zip
}

/// #1064 (different-column-width-excel2010, tdf124525): an empty thumbnail
/// written as deflate with no stream bytes and a directory entry do not
/// make the package corrupt.
#[test]
fn empty_deflate_and_directory_entries_load() {
    let mut parts = package("/");
    parts.push(("docProps/thumbnail.wmf".into(), Vec::new()));
    parts.push(("xl/media/".into(), Vec::new()));
    let zip = deflate_method(write_zip(&parts), "docProps/thumbnail.wmf");
    let zip = deflate_method(zip, "xl/media/");
    let pkg = load_xlsx(&zip).expect("loads");
    assert_eq!(a1(&pkg), &CellValue::Number(42.0));
    assert_eq!(pkg.part("docProps/thumbnail.wmf"), Some(&[][..]));
    assert!(load_xlsx(&save_xlsx(&pkg)).is_ok());
}

/// The entry names of `zip`, sorted.
fn entry_names(zip: &[u8]) -> Vec<String> {
    let arc = ZipArchive::open(zip).unwrap();
    let mut names: Vec<String> = arc.entries().iter().map(|e| e.name.clone()).collect();
    names.sort();
    names
}

/// [`package`] with the directory entries tdf124525.xlsx has: empty, deflate,
/// no trailing `/` (only their ZIP attributes call them directories), plus a
/// trailing-`/` one and an empty `xl/embeddings` with nothing in it.
fn with_directory_entries() -> Vec<u8> {
    let mut parts = package("/");
    let dirs = [
        "_rels",
        "xl",
        "xl/_rels",
        "xl/theme",
        "xl/embeddings",
        "xl/media/",
    ];
    for (i, dir) in dirs.into_iter().enumerate() {
        parts.insert(i * 2, (dir.into(), Vec::new()));
    }
    dirs.into_iter()
        .fold(write_zip(&parts), |zip, dir| deflate_method(zip, dir))
}

/// #1156: a directory entry is no part, so a save does not write it back as
/// an empty part with no content type (Excel's repair prompt).
#[test]
fn directory_entries_are_no_parts() {
    let zip = with_directory_entries();
    let pkg = load_xlsx(&zip).expect("loads");
    assert_eq!(a1(&pkg), &CellValue::Number(42.0));
    let mut names = pkg.part_names();
    names.sort();
    let mut real: Vec<String> = package("/").into_iter().map(|(n, _)| n).collect();
    real.sort();
    assert_eq!(names, real);
    let plain = save_xlsx(&load_xlsx(&write_zip(&package("/"))).unwrap());
    assert_eq!(entry_names(&save_xlsx(&pkg)), entry_names(&plain));
}

/// #1156: the repair loader reads the same parts from an undamaged package.
#[test]
fn the_repair_loader_drops_directory_entries_too() {
    let (pkg, repairs) = load_xlsx_repair(&with_directory_entries()).expect("loads");
    assert_eq!(repairs, Repairs::default());
    let plain = load_xlsx(&write_zip(&package("/"))).unwrap();
    assert_eq!(pkg.part_names(), plain.part_names());
}

/// #1156: only an empty entry is a directory, and only when an entry lies
/// under it or it has no extension; `xl/a.b` is no directory of
/// `xl/a.bc.xml`.
#[test]
fn which_empty_entries_are_directories() {
    let dirs: HashSet<&str> = ["xl", "xl/a.b", "_rels"].into_iter().collect();
    let dir = |name: &str, size: u64| is_directory_entry(name, size, &dirs);
    assert!(dir("xl/media/", 0));
    assert!(dir("xl", 0));
    assert!(dir("xl/a.b", 0), "an entry lies under it");
    assert!(dir("xl/embeddings", 0), "no extension");
    assert!(!dir("xl/a.bc", 0), "an extension and nothing under it");
    assert!(!dir("docProps/thumbnail.wmf", 0));
    assert!(!dir("xl", 3), "not empty: a part, however odd");
    assert!(!dir("_rels/.rels", 0));
}

/// [`package`] with its sheet written as tdf76115.xlsx writes it: in
/// ISO-8859-1, with Latin-1 inline strings.
fn latin1_package() -> Vec<(String, Vec<u8>)> {
    let mut sheet = format!(
        "<?xml version=\"1.0\" encoding=\"ISO-8859-1\" standalone=\"yes\"?><worksheet xmlns=\"{SML}\"><sheetData><row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t><![CDATA[S"
    )
    .into_bytes();
    sheet.extend_from_slice(b"\xe9rie N\xba]]></t></is></c></row></sheetData></worksheet>");
    let mut parts = package("\\");
    for (name, bytes) in &mut parts {
        if name == "xl\\sheet1.xml" {
            *bytes = sheet.clone();
        }
    }
    parts
}

/// The sheet's A1 text, and the saved sheet part's text (which must be
/// UTF-8 and say so), after a save and reload of `pkg`.
fn latin1_round_trip(pkg: &SheetPackage) {
    let text = CellValue::Text("S\u{e9}rie N\u{ba}".into());
    assert_eq!(a1(pkg), &text);
    let saved = save_xlsx(pkg);
    let zip = ZipArchive::open(&saved).unwrap();
    let sheet = String::from_utf8(zip.read("xl/sheet1.xml").unwrap()).expect("UTF-8");
    assert!(!sheet.contains("8859"), "{sheet}");
    let back = load_xlsx(&saved).expect("the save reloads");
    assert_eq!(a1(&back), &text);
}

/// #1108: a part declaring ISO-8859-1 is read by its declaration, not as
/// UTF-8 with every accented letter replaced, and is saved as UTF-8.
#[test]
fn latin1_sheet_text_survives_a_save() {
    let zip = write_zip(&latin1_package());
    latin1_round_trip(&load_xlsx(&zip).expect("loads"));
    let (pkg, repairs) = load_xlsx_repair(&zip).expect("repair loads");
    assert!(repairs.is_empty());
    latin1_round_trip(&pkg);
}

/// The same through repair's own path: one entry damaged.
#[test]
fn latin1_sheet_text_survives_a_repair() {
    let zip = write_zip(&latin1_package());
    let theme = ZipArchive::open(&zip)
        .unwrap()
        .find("xl\\theme\\theme1.xml")
        .unwrap()
        .local_offset as usize;
    let mut damaged = zip.clone();
    damaged[theme..theme + 4].copy_from_slice(&[0; 4]);
    let (pkg, repairs) = load_xlsx_repair(&damaged).expect("repairs");
    assert_eq!(repairs.dropped, ["xl/theme/theme1.xml"]);
    latin1_round_trip(&pkg);
}

/// #1108 r1: a shared-strings part declaring ISO-8859-1 but holding only
/// ASCII is read as declaring UTF-8, so the non-ASCII text a save appends
/// to it is what the part declares.
#[test]
fn ascii_latin1_part_declares_utf8_after_an_addition() {
    let mut parts = package("/");
    let sst = format!(
        "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><sst xmlns=\"{SML}\" count=\"1\" uniqueCount=\"1\"><si><t>plain</t></si></sst>"
    );
    parts.push(("xl/sharedStrings.xml".into(), sst.into_bytes()));
    for (name, bytes) in &mut parts {
        if name == "xl/_rels/workbook.xml.rels" {
            let rels = String::from_utf8(bytes.clone()).unwrap().replace(
                "</Relationships>",
                &format!("<Relationship Id=\"rId9\" Type=\"{R}/sharedStrings\" Target=\"sharedStrings.xml\"/></Relationships>"),
            );
            *bytes = rels.into_bytes();
        }
    }
    let mut pkg = load_xlsx(&write_zip(&parts)).expect("loads");
    pkg.workbook.sheets[0].set_cell(0, 1, crate::sheet::Cell::text("caf\u{e9}"));
    let saved = save_xlsx(&pkg);
    let zip = ZipArchive::open(&saved).unwrap();
    let sst = String::from_utf8(zip.read("xl/sharedStrings.xml").unwrap()).expect("UTF-8");
    assert!(sst.contains("caf\u{e9}"), "{sst}");
    assert!(
        !sst.contains("8859"),
        "UTF-8 text under a Latin-1 declaration: {sst}"
    );
}
