//! Which ZIP entries of an OPC package are parts, and which are directories.
//!
//! Some writers mark `_rels`, `xl` and the like as directories only in their
//! ZIP attributes: the entries are empty and carry no trailing `/`
//! (tdf124525.xlsx, #1156). Kept as parts, a save writes them with no content
//! type, which Office repairs (#1167). One rule for every loader.

use std::collections::HashSet;

use crate::xml::{Event, XmlParser};
use crate::zip::ZipArchive;

/// The most uncompressed bytes, in total, of the `[Content_Types].xml` and
/// `.rels` entries that are read to find the names in them. An entry that
/// would pass it names nothing, so a hostile package cannot make the
/// classification inflate much, however many such entries it lists (#1167).
pub const MAX_NAMING_PART: u64 = 8 << 20;

/// For each entry of `zip`, in order, whether it is a part. `names` are the
/// entries' names with `/` separators, one per entry.
///
/// A name ending with `/` is a directory. So is an empty entry that either
/// has an entry under it (`xl` beside `xl/workbook.xml`) or has no extension
/// (an empty `xl/media`) and is named by nothing. An empty entry with an
/// extension (`docProps/thumbnail.wmf`), or one an `Override` in
/// `[Content_Types].xml` or an internal relationship names, is a part.
pub fn part_flags(zip: &ZipArchive, names: &[String]) -> Vec<bool> {
    let dirs: HashSet<&str> = names
        .iter()
        .flat_map(|n| n.match_indices('/').map(|(i, _)| &n[..i]))
        .collect();
    let named = named_parts(zip, names);
    zip.entries()
        .iter()
        .zip(names)
        .map(|(e, name)| !is_directory_entry(name, e.uncomp_size, &dirs, &named))
        .collect()
}

/// Whether the entry `name` (with `/` separators) of `size` bytes is a
/// directory. `named` holds the part names, in lower case, the package names;
/// `dirs` every directory an entry name lies in.
fn is_directory_entry(
    name: &str,
    size: u64,
    dirs: &HashSet<&str>,
    named: &HashSet<String>,
) -> bool {
    let leaf = name.rsplit('/').next().unwrap_or(name);
    let unnamed = || !leaf.contains('.') && !named.contains(&name.to_ascii_lowercase());
    name.ends_with('/') || (size == 0 && (dirs.contains(name) || unnamed()))
}

/// The part names, in lower case, that the package names: an `Override` in
/// `[Content_Types].xml` or an internal relationship's target. A stream that
/// cannot be read names nothing.
fn named_parts(zip: &ZipArchive, names: &[String]) -> HashSet<String> {
    let mut named = HashSet::new();
    let mut read = 0u64;
    for (e, name) in zip.entries().iter().zip(names) {
        let lower = name.to_ascii_lowercase();
        let source = match lower.rsplit_once('/') {
            Some((dir, _)) if lower.ends_with(".rels") => match dir.rsplit_once('/') {
                Some((parent, "_rels")) => Some(&name[..parent.len()]),
                None if dir == "_rels" => Some(""),
                _ => None,
            },
            _ => None,
        };
        if source.is_none() && lower != "[content_types].xml" {
            continue;
        }
        read = read.saturating_add(e.uncomp_size);
        if read > MAX_NAMING_PART {
            continue;
        }
        let Some(bytes) = zip.extract(e) else {
            continue;
        };
        let xml = String::from_utf8_lossy(&bytes);
        let mut p = XmlParser::new(&xml);
        loop {
            match p.next() {
                Event::Start => match (source, local(p.name())) {
                    (Some(dir), "Relationship") if p.attr("TargetMode").is_empty() => {
                        let target = decode(p.attr("Target")).replace('\\', "/");
                        named.insert(resolve_relative(dir, &target).to_ascii_lowercase());
                    }
                    (None, "Override") => {
                        named.insert(
                            decode(p.attr("PartName"))
                                .trim_start_matches('/')
                                .to_ascii_lowercase(),
                        );
                    }
                    _ => {}
                },
                Event::Eof => break,
                _ => {}
            }
        }
    }
    named
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn decode(raw: &str) -> String {
    let mut s = String::new();
    XmlParser::append_decoded(raw, &mut s);
    s
}

/// A relationship target relative to the directory `dir` of its source.
fn resolve_relative(dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zipwrite::write_zip;

    const TYPES: &str = "<Types><Default Extension=\"xml\" ContentType=\"a\"/>\
        <Override PartName=\"/word/empty\" ContentType=\"b\"/></Types>";
    const RELS: &str = "<Relationships>\
        <Relationship Id=\"1\" Type=\"t\" Target=\"media/linked\"/>\
        <Relationship Id=\"2\" Type=\"t\" Target=\"media/ext\" TargetMode=\"External\"/>\
        </Relationships>";

    /// Flags for the entries `(name, bytes)`, keyed by name.
    fn flags(entries: &[(&str, &str)]) -> Vec<(String, bool)> {
        let entries: Vec<(String, Vec<u8>)> = entries
            .iter()
            .map(|(n, b)| (n.to_string(), b.as_bytes().to_vec()))
            .collect();
        let data = write_zip(&entries);
        let zip = ZipArchive::open(&data).unwrap();
        let names: Vec<String> = zip.entries().iter().map(|e| e.name.clone()).collect();
        let flags = part_flags(&zip, &names);
        names.into_iter().zip(flags).collect()
    }

    fn is_part(flags: &[(String, bool)], name: &str) -> bool {
        flags.iter().find(|(n, _)| n == name).unwrap().1
    }

    /// A relationships part over [`MAX_NAMING_PART`] names nothing (it is not
    /// even read); the same part under the limit names its target. The limit
    /// is a total: small parts name until their sizes pass it.
    #[test]
    fn an_oversize_naming_part_names_nothing() {
        let rels = |pad: usize| {
            format!(
                "<Relationships><Relationship Id=\"1\" Type=\"t\" Target=\"x\"/>{}</Relationships>",
                " ".repeat(pad)
            )
        };
        let small = rels(0);
        let big = rels(MAX_NAMING_PART as usize);
        let f = flags(&[("_rels/.rels", &small), ("x", "")]);
        assert!(is_part(&f, "x"), "named");
        let f = flags(&[("_rels/.rels", &big), ("x", "")]);
        assert!(!is_part(&f, "x"), "the oversize part names nothing");
    }

    #[test]
    fn naming_parts_are_bounded_in_total() {
        let rels = |target: &str, pad: usize| {
            format!(
                "<Relationships><Relationship Id=\"1\" Type=\"t\" Target=\"/{target}\"/>{}</Relationships>",
                " ".repeat(pad)
            )
        };
        let pad = MAX_NAMING_PART as usize / 2 - 200;
        let (a, b, c) = (rels("a", pad), rels("b", pad), rels("c", pad));
        let f = flags(&[
            ("_rels/.rels", &a),
            ("d1/_rels/x.rels", &b),
            ("d2/_rels/x.rels", &c),
            ("a", ""),
            ("b", ""),
            ("c", ""),
        ]);
        assert!(is_part(&f, "a") && is_part(&f, "b"), "within the total");
        assert!(!is_part(&f, "c"), "past the total");
    }

    #[test]
    fn the_rule_tells_directories_from_parts() {
        let f = flags(&[
            ("[Content_Types].xml", TYPES),
            ("word/_rels/document.xml.rels", RELS),
            ("word/document.xml", "<d/>"),
            ("word", ""),
            ("word/_rels", ""),
            ("word/media/", ""),
            ("customXml", ""),
            ("word/thumb.wmf", ""),
            ("word/empty", ""),
            ("word/media/linked", ""),
            ("word/media/ext", ""),
            ("word/media/full", "x"),
            ("word/media/none", ""),
        ]);
        for dir in [
            "word",
            "word/_rels",
            "word/media/",
            "customXml",
            "word/media/ext",
            "word/media/none",
        ] {
            assert!(!is_part(&f, dir), "{dir} is a directory");
        }
        for part in [
            "[Content_Types].xml",
            "word/document.xml",
            "word/thumb.wmf",
            "word/empty",
            "word/media/linked",
            "word/media/full",
        ] {
            assert!(is_part(&f, part), "{part} is a part");
        }
    }

    /// #1156: only an empty entry is a directory, and only when an entry lies
    /// under it or it has no extension; `xl/a.b` is no directory of
    /// `xl/a.bc.xml`.
    #[test]
    fn which_empty_entries_are_directories() {
        let dirs: HashSet<&str> = ["xl", "xl/a.b", "_rels"].into_iter().collect();
        let named: HashSet<String> = ["xl/customdata".to_string()].into_iter().collect();
        let dir = |name: &str, size: u64| is_directory_entry(name, size, &dirs, &named);
        assert!(dir("xl/media/", 0));
        assert!(dir("xl", 0));
        assert!(dir("xl/a.b", 0), "an entry lies under it");
        assert!(dir("xl/embeddings", 0), "no extension");
        assert!(!dir("xl/customData", 0), "no extension, but named");
        assert!(dir("xl/media/", 0) && dir("xl", 0), "named or not");
        assert!(!dir("xl/a.bc", 0), "an extension and nothing under it");
        assert!(!dir("docProps/thumbnail.wmf", 0));
        assert!(!dir("xl", 3), "not empty: a part, however odd");
        assert!(!dir("_rels/.rels", 0));
    }
}
