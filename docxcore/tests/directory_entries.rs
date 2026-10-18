//! #1167: a ZIP directory entry is no part. A writer may mark `_rels`, `word`
//! and the like as directories only in the ZIP attributes (empty, no trailing
//! `/`, as tdf124525.xlsx does, #1156); kept, a save wrote each as a part with
//! no content type, which Word repairs.

use docxcore::model::Document;
use docxcore::package::{load_package, new_package, save_package};
use docxcore::zip::ZipArchive;
use docxcore::zipwrite::write_zip;

fn entries(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
    let arc = ZipArchive::open(zip).unwrap();
    arc.entries()
        .iter()
        .map(|e| (e.name.clone(), arc.extract(e).unwrap()))
        .collect()
}

fn names(zip: &[u8]) -> Vec<String> {
    let mut names: Vec<String> = entries(zip).into_iter().map(|(n, _)| n).collect();
    names.sort();
    names
}

fn baseline() -> Vec<u8> {
    save_package(&new_package(Document::default()))
}

/// `baseline()` with `extra` entries added in front.
fn with(extra: &[(&str, &[u8])]) -> Vec<u8> {
    let mut all: Vec<(String, Vec<u8>)> = extra
        .iter()
        .map(|(n, b)| (n.to_string(), b.to_vec()))
        .collect();
    all.extend(entries(&baseline()));
    write_zip(&all)
}

#[test]
fn directory_entries_are_no_parts() {
    let plain = baseline();
    let dirs = with(&[
        ("_rels", b""),
        ("word", b""),
        ("word/_rels", b""),
        ("word/media/", b""),
        ("customXml", b""),
    ]);
    let saved = save_package(&load_package(&dirs).unwrap());
    assert_eq!(names(&saved), names(&plain));
}

#[test]
fn an_empty_part_with_an_extension_or_a_name_is_kept() {
    let types = String::from_utf8(
        entries(&baseline())
            .into_iter()
            .find(|(n, _)| n == "[Content_Types].xml")
            .unwrap()
            .1,
    )
    .unwrap()
    .replace(
        "</Types>",
        "<Override PartName=\"/word/named\" ContentType=\"application/x-a\"/></Types>",
    );
    let mut all: Vec<(String, Vec<u8>)> = entries(&baseline())
        .into_iter()
        .filter(|(n, _)| n != "[Content_Types].xml")
        .collect();
    all.push(("[Content_Types].xml".into(), types.into_bytes()));
    all.push(("docProps/thumbnail.wmf".into(), Vec::new()));
    all.push(("word/named".into(), Vec::new()));
    let saved = save_package(&load_package(&write_zip(&all)).unwrap());
    let names = names(&saved);
    assert!(
        names.contains(&"docProps/thumbnail.wmf".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"word/named".to_string()), "{names:?}");
}
