//! The native `.yppx` package format.
//!
//! The project-scheduling analog of `.docx`/`.xlsx`: where a raw MSPDI file is a
//! bare XML document, `.yppx` is a proper **OPC package** — a ZIP container with
//! a `[Content_Types].xml` and a main `project.xml` part — built on the same
//! `opccore` plumbing the Office formats use. That buys compression, a stable
//! container we can grow (add parts for views, baselines, resources art later)
//! without changing the outer shape, and a clean `doc→docx`, `mpp→yppx` story.
//!
//! For now the `project.xml` part is MSPDI-compatible, so `.yppx` losslessly
//! carries everything the model holds and stays interoperable: unzip a `.yppx`,
//! rename `project.xml`, and MS Project can open it.

use crate::model::{PackageParts, Project};
use crate::mspdi::{read_mspdi, write_mspdi};
use opccore::xml::{Event, XmlParser};
use opccore::zip::{ZipArchive, ZipEntry};
use opccore::zipwrite::write_zip;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const CONTENT_TYPES_PART: &str = "[Content_Types].xml";
const PROJECT_CONTENT_TYPE: &str = "application/vnd.yppx.project+xml";
const CONTENT_TYPES_START: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
    "<Default Extension=\"xml\" ContentType=\"",
);
const CONTENT_TYPES_END: &str = "</Types>\n";

/// Name of the main document part inside the package.
pub const MAIN_PART: &str = "project.xml";

/// Resolve a project save destination for both the terminal editor and suite.
/// Extensionless names become `.yppx`; only `.yppx` and `.xml` are writable.
pub fn save_target(path: &Path) -> Result<PathBuf, String> {
    if path.extension().is_none() {
        Ok(path.with_extension("yppx"))
    } else if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("yppx") || ext.eq_ignore_ascii_case("xml"))
    {
        Ok(path.to_path_buf())
    } else {
        Err("Project schedules can only be saved as .yppx or .xml (MSPDI)".into())
    }
}

/// Serialize a [`Project`] into a `.yppx` package (bytes of a ZIP container).
pub fn write_yppx(proj: &Project) -> Result<Vec<u8>, String> {
    if !proj.package.unreadable.is_empty() {
        return Err(format!(
            "cannot save as .yppx: part(s) {} could not be read and would be lost; save as .xml (MSPDI) instead",
            proj.package.unreadable.join(", ")
        ));
    }
    let mut entries = vec![
        (
            CONTENT_TYPES_PART.to_string(),
            build_content_types(&proj.package).into_bytes(),
        ),
        (MAIN_PART.to_string(), write_mspdi(proj).into_bytes()),
    ];
    let mut seen = HashSet::from([
        CONTENT_TYPES_PART.to_ascii_lowercase(),
        MAIN_PART.to_ascii_lowercase(),
    ]);
    for (name, bytes) in &proj.package.parts {
        if !name.ends_with('/') && seen.insert(name.to_ascii_lowercase()) {
            entries.push((name.clone(), bytes.to_vec()));
        }
    }
    Ok(write_zip(&entries))
}

/// XML parts default to the project type, and the main part is `/project.xml`.
/// Insert retained declarations before closing the map.
fn build_content_types(package: &PackageParts) -> String {
    let mut content_types = format!("{CONTENT_TYPES_START}{PROJECT_CONTENT_TYPE}\"/>");
    for (extension, content_type) in &package.defaults {
        if extension.eq_ignore_ascii_case("xml") {
            continue;
        }
        content_types.push_str(&format!(
            "<Default Extension=\"{}\" ContentType=\"{}\"/>",
            escape_attr(extension),
            escape_attr(content_type)
        ));
    }
    for (part_name, content_type) in &package.overrides {
        if part_name
            .trim_start_matches('/')
            .eq_ignore_ascii_case(MAIN_PART)
        {
            continue;
        }
        content_types.push_str(&format!(
            "<Override PartName=\"{}\" ContentType=\"{}\"/>",
            escape_attr(part_name),
            escape_attr(content_type)
        ));
    }
    content_types.push_str(CONTENT_TYPES_END);
    content_types
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn decoded(raw: &str) -> String {
    let mut value = String::new();
    XmlParser::append_decoded(raw, &mut value);
    value
}

const CT_NS: &str = "http://schemas.openxmlformats.org/package/2006/content-types";

/// The local name of the current start element when it is a content-types
/// element: in the OPC content-types namespace, or in no namespace (the
/// unqualified form hand-written maps and the existing tests use; an
/// unqualified child of a prefixed root is accepted the same way). `Err` means the prefix is unbound, which
/// makes the map namespace-malformed.
fn content_types_name<'a>(parser: &XmlParser<'a>) -> Result<Option<&'a str>, ()> {
    let name = parser.name();
    let (declaration, local) = match name.split_once(':') {
        Some((prefix, local)) => (Some(prefix), local),
        None => (None, name),
    };
    let namespace = parser
        .namespace_attrs()
        .iter()
        .find(|attr| match declaration {
            Some(prefix) => attr.name.strip_prefix("xmlns:") == Some(prefix),
            None => attr.name == "xmlns",
        })
        .map(|attr| attr.value);
    match (declaration, namespace) {
        (Some(_), None | Some("")) => Err(()),
        (_, None | Some("") | Some(CT_NS)) => Ok(Some(local)),
        _ => Ok(None),
    }
}

fn decode_content_types(bytes: &[u8]) -> Option<String> {
    if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        decode_utf16(bytes, u16::from_le_bytes)
    } else if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        decode_utf16(bytes, u16::from_be_bytes)
    } else {
        std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
            .ok()
            .map(str::to_owned)
    }
}

fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Option<String> {
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    String::from_utf16(&pairs.iter().copied().map(unit).collect::<Vec<_>>()).ok()
}

/// Parse a complete content-type map. A damaged map cannot safely supply a
/// partial set of declarations, so callers keep the parts but discard its map.
fn read_content_types(xml: &str, package: &mut PackageParts) -> Option<()> {
    let mut parser = XmlParser::new(xml);
    let mut stack = Vec::new();
    let mut defaults = Vec::new();
    let mut overrides = Vec::new();
    let mut xml_default = None;
    let mut root_closed = false;
    loop {
        match parser.next() {
            Event::Start => {
                let name = content_types_name(&parser).ok()?;
                if stack.is_empty() && (root_closed || name != Some("Types")) {
                    return None;
                }
                if stack.len() == 1 && name == Some("Default") {
                    let ext = decoded(parser.attr("Extension"));
                    let kind = decoded(parser.attr("ContentType"));
                    if !ext.is_empty() && !kind.is_empty() {
                        if ext.eq_ignore_ascii_case("xml") {
                            xml_default = Some(kind);
                        } else {
                            defaults.push((ext, kind));
                        }
                    }
                } else if stack.len() == 1 && name == Some("Override") {
                    let part = decoded(parser.attr("PartName"));
                    let kind = decoded(parser.attr("ContentType"));
                    if !part.is_empty() && !kind.is_empty() {
                        overrides.push((part, kind));
                    }
                }
                stack.push(parser.name().to_string());
            }
            Event::End => {
                if stack.pop().as_deref() != Some(parser.name()) {
                    return None;
                }
                if stack.is_empty() {
                    root_closed = true;
                }
            }
            Event::Eof => break,
            Event::Text => {
                if stack.is_empty() && !parser.text().trim().is_empty() {
                    return None;
                }
            }
        }
    }
    if !root_closed || !stack.is_empty() || parser.is_malformed() {
        return None;
    }
    let kept = |part: &str| {
        package
            .parts
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(part.trim_start_matches('/')))
    };
    overrides.retain(|(part, _)| kept(part));
    if let Some(kind) = xml_default {
        if kind != PROJECT_CONTENT_TYPE {
            for (name, _) in &package.parts {
                if name
                    .rsplit_once('.')
                    .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("xml"))
                    && !overrides
                        .iter()
                        .any(|(part, _)| part.trim_start_matches('/').eq_ignore_ascii_case(name))
                {
                    overrides.push((format!("/{name}"), kind.clone()));
                }
            }
        }
    }
    package.defaults = defaults;
    package.overrides = overrides;
    Some(())
}

/// Budget, in bytes, for the sum of the declared uncompressed sizes of every
/// part [`read_yppx`] extracts on open (the main part, the content-type map and
/// every retained extra part). A package over it is refused before anything is
/// extracted. Our own writer stores parts uncompressed, so their size is
/// bounded by the file's; the budget guards against deflated packages from
/// other tools whose small entries would inflate to gigabytes. The real peak
/// is several times the budget once the parts are copied and the project
/// model is built from `project.xml`.
pub const MAX_PACKAGE_UNCOMPRESSED: u64 = 256 * 1024 * 1024;

/// Read a `.yppx` package back into a [`Project`].
pub fn read_yppx(bytes: &[u8]) -> Result<Project, String> {
    read_yppx_within(bytes, MAX_PACKAGE_UNCOMPRESSED)
}

/// The uncompressed bytes `entries` declare, in total. ZIP64 sizes are 64-bit,
/// so the sum saturates rather than overflow.
fn declared_bytes<'a>(entries: impl Iterator<Item = &'a ZipEntry>) -> u64 {
    entries.fold(0, |total, entry| total.saturating_add(entry.uncomp_size))
}

fn read_yppx_within(bytes: &[u8], budget: u64) -> Result<Project, String> {
    let zip = ZipArchive::open(bytes).ok_or("not a valid .yppx (ZIP) container")?;
    let missing = || format!(".yppx package is missing its {MAIN_PART} part");
    let main = zip
        .entries()
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(MAIN_PART))
        .ok_or_else(missing)?;
    let mut seen = HashSet::new();
    let extras: Vec<_> = zip
        .entries()
        .iter()
        .filter(|entry| {
            let name = &entry.name;
            !(name.ends_with('/')
                || name.eq_ignore_ascii_case(CONTENT_TYPES_PART)
                || name.eq_ignore_ascii_case(MAIN_PART)
                || !seen.insert(name.to_ascii_lowercase()))
        })
        .collect();
    let content_types = zip
        .entries()
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(CONTENT_TYPES_PART));
    let total = declared_bytes(
        std::iter::once(main)
            .chain(extras.iter().copied())
            .chain(content_types),
    );
    if total > budget {
        return Err(format!(
            ".yppx package too large: its parts unpack to {total} bytes, over the {budget}-byte budget"
        ));
    }
    let part = zip.extract(main).ok_or_else(missing)?;
    let xml = String::from_utf8(part).map_err(|_| format!("{MAIN_PART} is not valid UTF-8"))?;
    let mut project = read_mspdi(&xml)?;
    for entry in extras {
        if let Some(bytes) = zip.extract(entry) {
            project
                .package
                .parts
                .push((entry.name.clone(), Arc::from(bytes)));
        } else {
            project.package.unreadable.push(entry.name.clone());
        }
    }
    if let Some(map) = content_types
        .and_then(|entry| zip.extract(entry))
        .and_then(|bytes| decode_content_types(&bytes))
    {
        read_content_types(&map, &mut project.package);
    }
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datetime::DateTime;
    use crate::model::*;

    const ORIGINAL_CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"xml\" ContentType=\"application/vnd.yppx.project+xml\"/></Types>\n";

    #[test]
    fn save_targets_add_native_extension_and_reject_lossy_formats() {
        assert_eq!(
            save_target(Path::new("dir/plan")).unwrap(),
            PathBuf::from("dir/plan.yppx")
        );
        for name in ["plan.yppx", "plan.YPPX", "plan.xml", "plan.XML"] {
            assert_eq!(save_target(Path::new(name)).unwrap(), PathBuf::from(name));
        }
        for name in ["plan.mpp", "plan.MPP", "plan.txt", "plan."] {
            assert_eq!(
                save_target(Path::new(name)).unwrap_err(),
                "Project schedules can only be saved as .yppx or .xml (MSPDI)"
            );
        }
    }

    /// #111: the package's MSPDI part tells Project to keep its durations.
    #[test]
    fn project_part_declares_durations_authoritative() {
        let bytes = write_yppx(&Project::default()).unwrap();
        let part = ZipArchive::open(&bytes).unwrap().read(MAIN_PART).unwrap();
        let xml = String::from_utf8(part).unwrap();
        let tag = "<ProjectExternallyEdited>0</ProjectExternallyEdited>";
        assert_eq!(xml.matches(tag).count(), 1);
        assert!(!xml.contains("<ProjectExternallyEdited>1"));
    }

    fn sample() -> Project {
        let mut a = Task {
            uid: 1,
            id: 1,
            name: "Design & build".into(),
            outline_level: 1,
            duration_min: 960,
            ..Task::default()
        };
        a.stored_start = Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0));
        let mut b = Task {
            uid: 2,
            id: 2,
            name: "Ship".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        };
        b.predecessors = vec![Predecessor::working(1, LinkType::FinishStart, 240)];
        Project {
            name: "Demo".into(),
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![a, b],
            ..Project::default()
        }
    }

    #[test]
    fn package_is_a_zip() {
        let bytes = write_yppx(&sample()).unwrap();
        assert_eq!(&bytes[..2], b"PK"); // ZIP local-file-header magic
        // and the container advertises the two expected parts
        let zip = ZipArchive::open(&bytes).unwrap();
        assert!(zip.read("[Content_Types].xml").is_some());
        assert!(zip.read(MAIN_PART).is_some());
        assert_eq!(zip.entries().len(), 2);
        assert_eq!(
            zip.read("[Content_Types].xml").unwrap(),
            ORIGINAL_CONTENT_TYPES.as_bytes()
        );
    }

    fn with_parts(map: &str, parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut entries = vec![
            ("[Content_Types].xml".to_string(), map.as_bytes().to_vec()),
            (MAIN_PART.to_string(), write_mspdi(&sample()).into_bytes()),
        ];
        entries.extend(
            parts
                .iter()
                .map(|(name, bytes)| (name.to_string(), bytes.to_vec())),
        );
        write_zip(&entries)
    }

    /// Offsets of the `index`th entry's central-directory and local headers.
    fn entry_headers(bytes: &[u8], index: usize) -> (usize, usize) {
        let eocd = bytes.windows(4).rposition(|s| s == b"PK\x05\x06").unwrap();
        let mut central =
            u32::from_le_bytes(bytes[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        for _ in 0..index {
            assert_eq!(&bytes[central..central + 4], b"PK\x01\x02");
            let name =
                u16::from_le_bytes(bytes[central + 28..central + 30].try_into().unwrap()) as usize;
            let extra =
                u16::from_le_bytes(bytes[central + 30..central + 32].try_into().unwrap()) as usize;
            let comment =
                u16::from_le_bytes(bytes[central + 32..central + 34].try_into().unwrap()) as usize;
            central += 46 + name + extra + comment;
        }
        assert_eq!(&bytes[central..central + 4], b"PK\x01\x02");
        let local =
            u32::from_le_bytes(bytes[central + 42..central + 46].try_into().unwrap()) as usize;
        assert_eq!(&bytes[local..local + 4], b"PK\x03\x04");
        (central, local)
    }

    /// Change one entry to Deflate64 in both ZIP headers, while leaving its
    /// bytes intact. Our ZIP reader can enumerate it but cannot extract it.
    fn unsupported_method(mut bytes: Vec<u8>, index: usize) -> Vec<u8> {
        let (central, local) = entry_headers(&bytes, index);
        bytes[local + 8..local + 10].copy_from_slice(&9u16.to_le_bytes());
        bytes[central + 10..central + 12].copy_from_slice(&9u16.to_le_bytes());
        bytes
    }

    /// Change one entry's declared uncompressed size in both ZIP headers.
    fn declared_size(mut bytes: Vec<u8>, index: usize, size: u32) -> Vec<u8> {
        let (central, local) = entry_headers(&bytes, index);
        bytes[local + 22..local + 26].copy_from_slice(&size.to_le_bytes());
        bytes[central + 24..central + 28].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    /// Sum of the declared uncompressed sizes of a package's entries.
    fn declared_total(bytes: &[u8]) -> u64 {
        ZipArchive::open(bytes)
            .unwrap()
            .entries()
            .iter()
            .map(|entry| entry.uncomp_size)
            .sum()
    }

    fn assert_too_large(result: Result<Project, String>) {
        let error = result.unwrap_err();
        assert!(error.contains("too large"), "{error}");
    }

    /// #1094: ZIP64 entries declare 64-bit sizes; their total saturates
    /// instead of overflowing past the budget check.
    #[test]
    fn zip64_declared_sizes_saturate() {
        let entry = |uncomp_size| ZipEntry {
            name: String::new(),
            method: 0,
            comp_size: 0,
            uncomp_size,
            local_offset: 0,
        };
        let entries = [entry(u64::MAX), entry(1), entry(5)];
        assert_eq!(declared_bytes(entries.iter()), u64::MAX);
        assert_eq!(declared_bytes(entries[1..].iter()), 6);
    }

    /// #451: a part declaring a huge size is refused before extraction; it
    /// used to be extracted (or listed unreadable) with no limit.
    #[test]
    fn huge_declared_extra_part_is_too_large() {
        let source = with_parts(ORIGINAL_CONTENT_TYPES, &[("media/big.bin", b"small")]);
        assert!(read_yppx(&source).is_ok());
        assert_too_large(read_yppx(&declared_size(source, 2, u32::MAX)));
    }

    #[test]
    fn oversized_main_part_is_too_large() {
        let source = with_parts(ORIGINAL_CONTENT_TYPES, &[]);
        let main = write_mspdi(&sample()).len() as u64;
        assert_too_large(read_yppx_within(&source, main - 1));
    }

    #[test]
    fn oversized_extra_part_is_too_large() {
        let small = with_parts(ORIGINAL_CONTENT_TYPES, &[("views.xml", b"<views/>")]);
        let budget = declared_total(&small);
        let big = vec![b'x'; 1024];
        let source = with_parts(ORIGINAL_CONTENT_TYPES, &[("views.xml", &big)]);
        assert!(read_yppx_within(&small, budget).is_ok());
        assert_too_large(read_yppx_within(&source, budget));
    }

    #[test]
    fn parts_over_budget_in_total_are_too_large() {
        let part = vec![b'x'; 100];
        let source = with_parts(
            ORIGINAL_CONTENT_TYPES,
            &[("a.bin", &part), ("b.bin", &part), ("c.bin", &part)],
        );
        let budget = declared_total(&source) - 1;
        // Each part fits on its own; only all three together are over.
        assert!(budget - 200 >= declared_total(&with_parts(ORIGINAL_CONTENT_TYPES, &[])));
        assert_too_large(read_yppx_within(&source, budget));
    }

    #[test]
    fn package_at_budget_opens() {
        let source = with_parts(ORIGINAL_CONTENT_TYPES, &[("views.xml", b"<views/>")]);
        let project = read_yppx_within(&source, declared_total(&source)).unwrap();
        assert_eq!(&*project.package.parts[0].1, b"<views/>");
    }

    #[test]
    fn skipped_entries_do_not_count_against_budget() {
        let kept = with_parts(ORIGINAL_CONTENT_TYPES, &[("views.xml", b"<views/>")]);
        let budget = declared_total(&kept);
        let big = vec![b'x'; 1024];
        let source = with_parts(
            ORIGINAL_CONTENT_TYPES,
            &[
                ("views.xml", b"<views/>"),
                ("VIEWS.XML", &big),
                ("Project.xml", &big),
                ("[content_types].XML", &big),
                ("media/", b""),
            ],
        );
        // A directory entry declaring bytes is skipped too.
        let source = declared_size(source, 6, 1024);
        let project = read_yppx_within(&source, budget).unwrap();
        assert_eq!(project.package.parts.len(), 1);
        assert_eq!(&*project.package.parts[0].1, b"<views/>");
    }

    #[test]
    fn missing_main_part_is_reported_before_budget() {
        let source = write_zip(&[
            (
                CONTENT_TYPES_PART.into(),
                ORIGINAL_CONTENT_TYPES.as_bytes().to_vec(),
            ),
            ("views.xml".into(), vec![b'x'; 1024]),
        ]);
        assert_eq!(
            read_yppx_within(&source, 0).unwrap_err(),
            ".yppx package is missing its project.xml part"
        );
    }

    #[test]
    fn unreadable_extra_parts_open_and_block_yppx_save() {
        let source = with_parts(
            ORIGINAL_CONTENT_TYPES,
            &[
                ("views.xml", b"readable"),
                ("media/one.bin", b"one"),
                ("media/two.bin", b"two"),
            ],
        );
        let source = unsupported_method(unsupported_method(source, 3), 4);
        let project = read_yppx(&source).unwrap();
        assert_eq!(project.tasks.len(), 2);
        assert_eq!(project.package.parts.len(), 1);
        assert_eq!(project.package.parts[0].0, "views.xml");
        assert_eq!(&*project.package.parts[0].1, b"readable");
        assert_eq!(
            project.package.unreadable,
            ["media/one.bin", "media/two.bin"]
        );
        let error = write_yppx(&project).unwrap_err();
        for expected in ["media/one.bin", "media/two.bin", "save as .xml"] {
            assert!(error.contains(expected), "{error}");
        }
        assert!(write_mspdi(&project).contains("Design &amp; build"));
    }

    #[test]
    fn unreadable_main_part_still_fails() {
        let source = unsupported_method(with_parts(ORIGINAL_CONTENT_TYPES, &[]), 1);
        assert_eq!(
            read_yppx(&source).unwrap_err(),
            ".yppx package is missing its project.xml part"
        );
    }

    #[test]
    fn unreadable_content_types_is_not_listed() {
        let map =
            r#"<Types><Override PartName="/views.xml" ContentType="application/x-views"/></Types>"#;
        let source = unsupported_method(with_parts(map, &[("views.xml", b"<views/>")]), 0);
        let project = read_yppx(&source).unwrap();
        assert!(project.package.unreadable.is_empty());
        assert!(project.package.overrides.is_empty());
        let output = write_yppx(&project).unwrap();
        let zip = ZipArchive::open(&output).unwrap();
        assert_eq!(
            zip.read(CONTENT_TYPES_PART).unwrap(),
            ORIGINAL_CONTENT_TYPES.as_bytes()
        );
        assert_eq!(zip.read("views.xml").unwrap(), b"<views/>");
    }

    #[test]
    fn unreadable_case_duplicate_is_skipped() {
        let source = write_zip(&[
            (
                CONTENT_TYPES_PART.into(),
                ORIGINAL_CONTENT_TYPES.as_bytes().to_vec(),
            ),
            (MAIN_PART.into(), write_mspdi(&sample()).into_bytes()),
            ("Views.xml".into(), b"first".to_vec()),
            ("views.XML".into(), b"second".to_vec()),
        ]);
        let project = read_yppx(&unsupported_method(source, 3)).unwrap();
        assert_eq!(project.package.parts.len(), 1);
        assert_eq!(&*project.package.parts[0].1, b"first");
        assert!(project.package.unreadable.is_empty());
    }

    #[test]
    fn keeps_views_binary_nested_parts_and_their_types() {
        let map = r#"<Types><Default Extension="xml" ContentType="application/vnd.yppx.project+xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/views.xml" ContentType="application/vnd.yppx.views+xml"/></Types>"#;
        let views = b"<views><table name=\"Entry\"/></views>";
        let png = b"\x89PNG\r\n\x1a\n\0\xff";
        let rels = b"<Relationships/>";
        let source = with_parts(
            map,
            &[
                ("views.xml", views),
                ("media/logo.png", png),
                ("_rels/.rels", rels),
            ],
        );
        let project = read_yppx(&source).unwrap();
        let output = write_yppx(&project).unwrap();
        let zip = ZipArchive::open(&output).unwrap();
        for (name, bytes) in [
            ("views.xml", views.as_slice()),
            ("media/logo.png", png.as_slice()),
            ("_rels/.rels", rels.as_slice()),
        ] {
            assert_eq!(zip.read(name).unwrap(), bytes);
        }
        let types = String::from_utf8(zip.read("[Content_Types].xml").unwrap()).unwrap();
        assert!(types.contains("<Default Extension=\"png\" ContentType=\"image/png\"/>"));
        assert!(types.contains(
            "<Override PartName=\"/views.xml\" ContentType=\"application/vnd.yppx.views+xml\"/>"
        ));
        assert!(!write_mspdi(&project).contains("<views"));
    }

    #[test]
    fn project_part_and_xml_type_are_regenerated() {
        let map = r#"<Types><Default Extension="xml" ContentType="application/vnd.future+xml"/><Override PartName="/project.xml" ContentType="application/vnd.future.project+xml"/></Types>"#;
        let source = with_parts(map, &[("views.xml", b"<views/>")]);
        let mut project = read_yppx(&source).unwrap();
        project.name = "Changed".into();
        let output = write_yppx(&project).unwrap();
        let zip = ZipArchive::open(&output).unwrap();
        let types = String::from_utf8(zip.read("[Content_Types].xml").unwrap()).unwrap();
        assert!(types.contains(
            "<Default Extension=\"xml\" ContentType=\"application/vnd.yppx.project+xml\"/>"
        ));
        assert!(types.contains(
            "<Override PartName=\"/views.xml\" ContentType=\"application/vnd.future+xml\"/>"
        ));
        assert!(!types.contains("future.project"));
        assert!(
            String::from_utf8(zip.read(MAIN_PART).unwrap())
                .unwrap()
                .contains("<Name>Changed</Name>")
        );
    }

    #[test]
    fn content_type_attributes_are_decoded_then_escaped() {
        let map = r#"<Types><Override PartName="/a&amp;b.xml" ContentType="application/x-a&amp;b"/></Types>"#;
        let source = with_parts(map, &[("a&b.xml", b"<a/>")]);
        let output = write_yppx(&read_yppx(&source).unwrap()).unwrap();
        let zip = ZipArchive::open(&output).unwrap();
        let types = String::from_utf8(zip.read("[Content_Types].xml").unwrap()).unwrap();
        assert!(types.contains(
            "<Override PartName=\"/a&amp;b.xml\" ContentType=\"application/x-a&amp;b\"/>"
        ));
        assert_eq!(zip.read("a&b.xml").unwrap(), b"<a/>");
    }

    #[test]
    fn malformed_content_types_keep_parts_without_partial_entries() {
        let map = r#"<Types><Override PartName="/views.xml" ContentType="application/x-views"/>"#;
        let source = with_parts(map, &[("views.xml", b"<views/>")]);
        let output = write_yppx(&read_yppx(&source).unwrap()).unwrap();
        let zip = ZipArchive::open(&output).unwrap();
        assert_eq!(zip.read("views.xml").unwrap(), b"<views/>");
        assert_eq!(
            zip.read("[Content_Types].xml").unwrap(),
            ORIGINAL_CONTENT_TYPES.as_bytes()
        );
    }

    #[test]
    fn prefixed_content_type_names_keep_overrides() {
        let map = r#"<ct:Types xmlns:ct="http://schemas.openxmlformats.org/package/2006/content-types"><ct:Override PartName="/views.xml" ContentType="application/x-views"/></ct:Types>"#;
        let output =
            write_yppx(&read_yppx(&with_parts(map, &[("views.xml", b"<views/>")])).unwrap())
                .unwrap();
        let types = String::from_utf8(
            ZipArchive::open(&output)
                .unwrap()
                .read(CONTENT_TYPES_PART)
                .unwrap(),
        )
        .unwrap();
        assert!(
            types.contains(
                "<Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/>"
            )
        );
    }

    #[test]
    fn utf16_content_type_map_keeps_overrides() {
        let map = r#"<?xml version="1.0" encoding="UTF-16"?><Types><Override PartName="/views.xml" ContentType="application/x-views"/></Types>"#;
        let mut encoded = vec![0xff, 0xfe];
        for unit in map.encode_utf16() {
            encoded.extend_from_slice(&unit.to_le_bytes());
        }
        let source = write_zip(&[
            (CONTENT_TYPES_PART.into(), encoded),
            (MAIN_PART.into(), write_mspdi(&sample()).into_bytes()),
            ("views.xml".into(), b"<views/>".to_vec()),
        ]);
        let output = write_yppx(&read_yppx(&source).unwrap()).unwrap();
        let types = String::from_utf8(
            ZipArchive::open(&output)
                .unwrap()
                .read(CONTENT_TYPES_PART)
                .unwrap(),
        )
        .unwrap();
        assert!(
            types.contains(
                "<Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/>"
            )
        );
    }

    #[test]
    fn utf8_bom_content_type_map_keeps_overrides() {
        let mut map = vec![0xef, 0xbb, 0xbf];
        map.extend_from_slice(b"<Types><Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/></Types>");
        let source = write_zip(&[
            (CONTENT_TYPES_PART.into(), map),
            (MAIN_PART.into(), write_mspdi(&sample()).into_bytes()),
            ("views.xml".into(), b"<views/>".to_vec()),
        ]);
        let output = write_yppx(&read_yppx(&source).unwrap()).unwrap();
        let types = String::from_utf8(
            ZipArchive::open(&output)
                .unwrap()
                .read(CONTENT_TYPES_PART)
                .unwrap(),
        )
        .unwrap();
        assert!(
            types.contains(
                "<Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/>"
            )
        );
    }

    #[test]
    fn uppercase_main_part_is_read_and_rewritten() {
        let source = write_zip(&[
            (
                CONTENT_TYPES_PART.into(),
                ORIGINAL_CONTENT_TYPES.as_bytes().to_vec(),
            ),
            ("PROJECT.XML".into(), write_mspdi(&sample()).into_bytes()),
        ]);
        let project = read_yppx(&source).unwrap();
        assert_eq!(project.name, "Demo");
        let output = write_yppx(&project).unwrap();
        let zip = ZipArchive::open(&output).unwrap();
        assert_eq!(zip.entries().len(), 2);
        assert!(zip.read(MAIN_PART).is_some());
    }

    #[test]
    fn mismatched_prefixes_discard_content_type_map() {
        let source = with_parts(
            "<a:Types><a:Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/></b:Types>",
            &[("views.xml", b"<views/>")],
        );
        let output = write_yppx(&read_yppx(&source).unwrap()).unwrap();
        let zip = ZipArchive::open(&output).unwrap();
        assert_eq!(
            zip.read(CONTENT_TYPES_PART).unwrap(),
            ORIGINAL_CONTENT_TYPES.as_bytes()
        );
        assert_eq!(zip.read("views.xml").unwrap(), b"<views/>");
    }

    #[test]
    fn lowercase_content_types_name_is_read() {
        let source = write_zip(&[
            ("[content_types].xml".into(), b"<Types><Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/></Types>".to_vec()),
            (MAIN_PART.into(), write_mspdi(&sample()).into_bytes()),
            ("views.xml".into(), b"<views/>".to_vec()),
        ]);
        let output = write_yppx(&read_yppx(&source).unwrap()).unwrap();
        let zip = ZipArchive::open(&output).unwrap();
        assert_eq!(zip.entries().len(), 3);
        assert!(
            String::from_utf8(zip.read(CONTENT_TYPES_PART).unwrap())
                .unwrap()
                .contains("application/x-views")
        );
    }

    #[test]
    fn junk_after_content_types_root_discards_the_map() {
        let source = with_parts(
            "<Types><Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/></Types>garbage",
            &[("views.xml", b"<views/>")],
        );
        let output = write_yppx(&read_yppx(&source).unwrap()).unwrap();
        let zip = ZipArchive::open(&output).unwrap();
        assert_eq!(
            zip.read(CONTENT_TYPES_PART).unwrap(),
            ORIGINAL_CONTENT_TYPES.as_bytes()
        );
        assert_eq!(zip.read("views.xml").unwrap(), b"<views/>");
    }

    /// Round-trip a package whose map is `content_types` and that keeps
    /// `views.xml`, returning the written map.
    fn round_trip_map(content_types: &str) -> String {
        let source = with_parts(content_types, &[("views.xml", b"<views/>")]);
        let output = write_yppx(&read_yppx(&source).unwrap()).unwrap();
        let zip = ZipArchive::open(&output).unwrap();
        assert_eq!(zip.read("views.xml").unwrap(), b"<views/>");
        String::from_utf8(zip.read(CONTENT_TYPES_PART).unwrap()).unwrap()
    }

    #[test]
    fn end_tag_tail_discards_content_type_map() {
        assert_eq!(
            round_trip_map(
                "<Types><Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/></Types bogus>"
            ),
            ORIGINAL_CONTENT_TYPES
        );
    }

    #[test]
    fn unterminated_markup_after_root_discards_content_type_map() {
        for tail in ["<!-- unterminated", "<?pi", "<"] {
            assert_eq!(
                round_trip_map(&format!(
                    "<Types><Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/></Types>{tail}"
                )),
                ORIGINAL_CONTENT_TYPES,
                "tail {tail:?}"
            );
        }
    }

    #[test]
    fn foreign_prefixed_override_is_ignored() {
        let map = round_trip_map(&format!(
            "<ct:Types xmlns:ct=\"{CT_NS}\" xmlns:x=\"urn:vendor\"><ct:Default Extension=\"png\" ContentType=\"image/png\"/><x:Override PartName=\"/views.xml\" ContentType=\"application/x-vendor\"/></ct:Types>"
        ));
        assert!(map.contains("<Default Extension=\"png\" ContentType=\"image/png\"/>"));
        assert!(!map.contains("application/x-vendor"));
        assert!(!map.contains("/views.xml"));
    }

    #[test]
    fn foreign_default_namespace_override_is_ignored() {
        let map = round_trip_map(&format!(
            "<Types xmlns=\"{CT_NS}\"><Default Extension=\"png\" ContentType=\"image/png\"/><Override xmlns=\"urn:vendor\" PartName=\"/views.xml\" ContentType=\"application/x-vendor\"/></Types>"
        ));
        assert!(map.contains("<Default Extension=\"png\" ContentType=\"image/png\"/>"));
        assert!(!map.contains("application/x-vendor"));
    }

    #[test]
    fn foreign_namespace_root_discards_content_type_map() {
        assert_eq!(
            round_trip_map(
                "<Types xmlns=\"urn:vendor\"><Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/></Types>"
            ),
            ORIGINAL_CONTENT_TYPES
        );
    }

    #[test]
    fn unbound_prefix_discards_content_type_map() {
        for map in [
            "<a:Types><Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/></a:Types>",
            "<Types><Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/><x:Default Extension=\"png\" ContentType=\"image/png\"/></Types>",
        ] {
            assert_eq!(round_trip_map(map), ORIGINAL_CONTENT_TYPES, "{map:?}");
        }
    }

    #[test]
    fn qualified_content_type_maps_are_still_read() {
        for map in [
            format!(
                "<Types xmlns=\"{CT_NS}\"><Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/></Types>"
            ),
            format!(
                "<ct:Types xmlns:ct=\"{CT_NS}\"><ct:Override PartName=\"/views.xml\" ContentType=\"application/x-views\"/></ct:Types>"
            ),
        ] {
            assert!(
                round_trip_map(&map).contains("application/x-views"),
                "{map:?}"
            );
        }
    }

    #[test]
    fn writer_filters_reserved_and_duplicate_part_names() {
        let mut project = sample();
        for name in [
            "PROJECT.XML",
            "[content_types].xml",
            "Views.xml",
            "views.XML",
        ] {
            project
                .package
                .parts
                .push((name.into(), Arc::from(b"opaque".as_slice())));
        }
        let zip = write_yppx(&project).unwrap();
        let archive = ZipArchive::open(&zip).unwrap();
        let names: Vec<_> = archive
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, [CONTENT_TYPES_PART, MAIN_PART, "Views.xml"]);
        assert_eq!(archive.read("Views.xml").unwrap(), b"opaque");
    }

    #[test]
    fn duplicate_input_part_name_keeps_first_bytes() {
        let source = write_zip(&[
            (
                CONTENT_TYPES_PART.into(),
                ORIGINAL_CONTENT_TYPES.as_bytes().to_vec(),
            ),
            (MAIN_PART.into(), write_mspdi(&sample()).into_bytes()),
            ("Views.xml".into(), b"first".to_vec()),
            ("views.XML".into(), b"second".to_vec()),
        ]);
        let project = read_yppx(&source).unwrap();
        assert_eq!(project.package.parts.len(), 1);
        assert_eq!(&*project.package.parts[0].1, b"first");
    }

    #[test]
    fn round_trips_through_package() {
        let orig = sample();
        let bytes = write_yppx(&orig).unwrap();
        let back = read_yppx(&bytes).unwrap();
        assert_eq!(back.name, orig.name);
        assert_eq!(back.tasks.len(), 2);
        assert_eq!(back.tasks[0].name, "Design & build");
        assert_eq!(back.tasks[0].duration_min, 960);
        assert_eq!(back.tasks[1].predecessors, orig.tasks[1].predecessors);
    }

    #[test]
    fn resource_rate_text_survives_package() {
        let huge = format!("1{}", "0".repeat(400));
        for name in ["StandardRate", "OvertimeRate", "CostPerUse"] {
            for value in [
                "9007199254740993",
                "0.12345678901234567890123456789",
                &huge,
                "+5",
                "-0.50",
                ".5",
                "5.",
                "007",
            ] {
                let xml = format!(
                    "<Project><Resources><Resource><{name}>{value}</{name}></Resource></Resources></Project>"
                );
                let project = read_mspdi(&xml).unwrap();
                let result = read_yppx(&write_yppx(&project).unwrap()).unwrap();
                let rate = match name {
                    "StandardRate" => result.resources[0].standard_rate.as_ref(),
                    "OvertimeRate" => result.resources[0].overtime_rate.as_ref(),
                    _ => result.resources[0].cost_per_use.as_ref(),
                };
                assert_eq!(rate.map(Rate::as_str), Some(value), "{name}");
            }
        }
    }

    #[test]
    fn rejects_non_package() {
        assert!(read_yppx(b"not a zip at all").is_err());
    }

    #[test]
    fn rejects_task_on_empty_calendar() {
        let mut proj = sample();
        proj.calendars[0].week = Default::default();
        let error = read_yppx(&write_yppx(&proj).unwrap()).unwrap_err();
        assert!(error.contains("calendar \"Standard\" (UID 1) has no working time"));
        assert!(error.contains("task \"Design & build\" (UID 1) cannot be scheduled"));
    }
}
