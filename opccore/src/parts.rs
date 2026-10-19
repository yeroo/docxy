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

/// The most bytes of relationship directories and targets resolved, in
/// total, while naming parts (#1167).
const MAX_RESOLVE_WORK: usize = 64 << 20;

/// For each entry of `zip`, in order, whether it is a part. `names` are the
/// entries' names with `/` separators, one per entry.
///
/// A name ending with `/` is a directory. So is an empty entry that either
/// has an entry under it (`xl` beside `xl/workbook.xml`) or has no extension
/// (an empty `xl/media`) and is named by nothing. An empty entry with an
/// extension (`docProps/thumbnail.wmf`), or one an `Override` in
/// `[Content_Types].xml` or an internal relationship names, is a part.
///
/// The work is bounded by the entries, not by the shapes of their names:
/// only an empty entry without an extension and with nothing under it can be
/// named into a part, and nothing else is looked up. Naming reads the first
/// `[Content_Types].xml` before any `.rels`, then the rest in ZIP order. It
/// stops when the declared sizes of what it read pass [`MAX_NAMING_PART`] in
/// total (an External relationship's part counts too), and a relationship
/// stops all naming once the directories and targets it resolved pass
/// `MAX_RESOLVE_WORK` (64 MiB) in total: later parts name nothing, so a
/// package with more than that in relationships can lose a part kept only by
/// them.
pub fn part_flags(zip: &ZipArchive, names: &[String]) -> Vec<bool> {
    let mut sorted: Vec<&str> = names.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let has_under = |name: &str| {
        let under = format!("{name}/");
        let at = sorted.partition_point(|n| *n < under.as_str());
        sorted.get(at).is_some_and(|n| n.starts_with(&under))
    };
    // Per entry: a directory already, a part for sure, or only a part if named.
    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Directory,
        Part,
        Candidate,
    }
    let kinds: Vec<Kind> = zip
        .entries()
        .iter()
        .zip(names)
        .map(|(e, name)| {
            let leaf = name.rsplit('/').next().unwrap_or(name);
            if name.ends_with('/') {
                Kind::Directory
            } else if e.uncomp_size != 0 {
                Kind::Part
            } else if has_under(name) {
                Kind::Directory
            } else if leaf.contains('.') {
                Kind::Part
            } else {
                Kind::Candidate
            }
        })
        .collect();
    let candidates: HashSet<String> = kinds
        .iter()
        .zip(names)
        .filter(|(kind, _)| **kind == Kind::Candidate)
        .map(|(_, name)| name.to_ascii_lowercase())
        .collect();
    let named = if candidates.is_empty() {
        HashSet::new()
    } else {
        named_parts(zip, names, &candidates)
    };
    kinds
        .iter()
        .zip(names)
        .map(|(kind, name)| match kind {
            Kind::Directory => false,
            Kind::Part => true,
            Kind::Candidate => named.contains(&name.to_ascii_lowercase()),
        })
        .collect()
}

/// Which of `candidates` (lower-case names) the package names: an `Override`
/// in `[Content_Types].xml` or an internal relationship's target. A stream
/// that cannot be read names nothing.
fn named_parts(
    zip: &ZipArchive,
    names: &[String],
    candidates: &HashSet<String>,
) -> HashSet<String> {
    // (entry, directory of a relationship source or None for content types)
    let mut sources: Vec<(&crate::zip::ZipEntry, Option<&str>)> = Vec::new();
    let mut first_types = true;
    let mut rels = Vec::new();
    for (e, name) in zip.entries().iter().zip(names) {
        let lower = name.to_ascii_lowercase();
        if lower == "[content_types].xml" {
            if first_types {
                sources.push((e, None));
                first_types = false;
            } else {
                rels.push((e, None));
            }
            continue;
        }
        let source = match lower.rsplit_once('/') {
            Some((dir, _)) if lower.ends_with(".rels") => match dir.rsplit_once('/') {
                Some((parent, "_rels")) => Some(&name[..parent.len()]),
                None if dir == "_rels" => Some(""),
                _ => None,
            },
            _ => None,
        };
        if source.is_some() {
            rels.push((e, source));
        }
    }
    sources.extend(rels);
    let mut named = HashSet::new();
    let mut read = 0u64;
    let mut work = 0usize;
    'sources: for (e, source) in sources {
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
                        work = work.saturating_add(dir.len() + target.len());
                        if work > MAX_RESOLVE_WORK {
                            break 'sources;
                        }
                        let target = resolve_relative(dir, &target).to_ascii_lowercase();
                        if candidates.contains(&target) {
                            named.insert(target);
                        }
                    }
                    (None, "Override") => {
                        let part = decode(p.attr("PartName"))
                            .trim_start_matches('/')
                            .to_ascii_lowercase();
                        if candidates.contains(&part) {
                            named.insert(part);
                        }
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
        let types = "<Types><Override PartName=\"/xl/customData\" ContentType=\"a\"/></Types>";
        let f = flags(&[
            ("[Content_Types].xml", types),
            ("xl/media/", ""),
            ("xl", ""),
            ("xl/a.b", ""),
            ("xl/a.b/c.xml", "x"),
            ("xl/embeddings", ""),
            ("xl/customData", ""),
            ("xl/a.bc", ""),
            ("docProps/thumbnail.wmf", ""),
            ("_rels/.rels", ""),
            ("docx", "x"),
            ("docx/inner.xml", "x"),
        ]);
        assert!(is_part(&f, "docx"), "not empty: a part, however odd");
        assert!(!is_part(&f, "xl/media/"));
        assert!(!is_part(&f, "xl"));
        assert!(!is_part(&f, "xl/a.b"), "an entry lies under it");
        assert!(!is_part(&f, "xl/embeddings"), "no extension");
        assert!(is_part(&f, "xl/customData"), "no extension, but named");
        assert!(is_part(&f, "xl/a.bc"), "an extension and nothing under it");
        assert!(is_part(&f, "docProps/thumbnail.wmf"));
        assert!(is_part(&f, "_rels/.rels"));
    }

    /// A name with tens of thousands of `/` costs no more than its length
    /// in comparisons; hashing each of its prefixes was quadratic.
    #[test]
    fn a_name_with_many_slashes_is_classified() {
        let deep = "a/".repeat(20_000) + "leaf";
        let dir = "a/".repeat(20_000).trim_end_matches('/').to_string();
        let f = flags(&[(&deep, "x"), (&dir, ""), ("b", "")]);
        assert!(is_part(&f, &deep));
        assert!(!is_part(&f, &dir), "an entry lies under it");
        assert!(!is_part(&f, "b"));
    }

    /// Relationships from a long directory name only entries that are
    /// candidates: thousands of targets that are no entry are dropped, the
    /// one that is an empty entry makes it a part.
    #[test]
    fn long_relationship_directories_name_only_candidates() {
        let dir = "d/".repeat(5_000) + "e";
        let mut rels = String::from("<Relationships>");
        for i in 0..2_000 {
            rels += &format!("<Relationship Id=\"{i}\" Type=\"t\" Target=\"no{i}\"/>");
        }
        rels += "<Relationship Id=\"x\" Type=\"t\" Target=\"yes\"/></Relationships>";
        let (source, owner, target) = (
            format!("{dir}/_rels/p.xml.rels"),
            format!("{dir}/p.xml"),
            format!("{dir}/yes"),
        );
        let f = flags(&[(&source, &rels), (&owner, "x"), (&target, ""), ("yes", "")]);
        assert!(is_part(&f, &target), "named by the relationship");
        assert!(!is_part(&f, "yes"), "a different part, named by nothing");
    }

    /// The first `[Content_Types].xml` is read before any `.rels`: a big
    /// rels ahead of it cannot use up the total first.
    #[test]
    fn content_types_are_read_before_a_big_rels() {
        let types = "<Types><Override PartName=\"/named\" ContentType=\"a\"/></Types>";
        let head = "<Relationships><Relationship Id=\"1\" Type=\"t\" Target=\"/other\"/>";
        let pad = MAX_NAMING_PART as usize - head.len() - "</Relationships>".len() - 10;
        let rels = format!("{head}{}</Relationships>", " ".repeat(pad));
        let f = flags(&[
            ("_rels/.rels", &rels),
            ("[Content_Types].xml", types),
            ("named", ""),
            ("other", ""),
        ]);
        assert!(is_part(&f, "named"), "the types were read first");
        assert!(!is_part(&f, "other"), "the rels was past the total");
    }
}
