//! How the ZIP container's entries become parts (#1108): separators other
//! writers use, and empty entries.

use super::*;
use crate::sheet::CellValue;
use opccore::zipwrite::write_zip;

const SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG_RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// A one-sheet workbook laid out as LibreOffice's tdf76115.xlsx is: the
/// sheet at `xl/sheet1.xml`, every relationship target absolute, and each
/// entry name written with `sep` between its segments.
fn package(sep: &str) -> Vec<(String, Vec<u8>)> {
    let ct = "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/></Types>";
    let root = format!(
        "<?xml version=\"1.0\"?><Relationships xmlns=\"{PKG_RELS}\"><Relationship Id=\"rId1\" Type = \"{R}/officeDocument\" Target=\"/xl/workbook.xml\"/></Relationships>"
    );
    let wb = format!(
        "<?xml version=\"1.0\"?><workbook xmlns=\"{SML}\" xmlns:r=\"{R}\"><sheets><sheet name=\"Plan1\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
    );
    let wb_rels = format!(
        "<?xml version=\"1.0\"?><Relationships xmlns=\"{PKG_RELS}\"><Relationship Id=\"rId1\" Type=\"{R}/worksheet\" Target=\"/xl/sheet1.xml\"/></Relationships>"
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
