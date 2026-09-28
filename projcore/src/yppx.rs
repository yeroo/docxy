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
use opccore::zip::ZipArchive;
use opccore::zipwrite::write_zip;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The single package-relationship content-type map. `xml` parts default to the
/// project content type; the main part lives at `/project.xml`.
const CONTENT_TYPES: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
    "<Default Extension=\"xml\" ContentType=\"application/vnd.yppx.project+xml\"/>",
    "</Types>\n",
);

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
pub fn write_yppx(proj: &Project) -> Vec<u8> {
    let mut content_types = CONTENT_TYPES.to_string();
    if !proj.package.defaults.is_empty() || !proj.package.overrides.is_empty() {
        let insertion = content_types.find("</Types>").unwrap();
        let mut extra = String::new();
        for (extension, content_type) in &proj.package.defaults {
            extra.push_str(&format!(
                "<Default Extension=\"{}\" ContentType=\"{}\"/>",
                escape_attr(extension),
                escape_attr(content_type)
            ));
        }
        for (part_name, content_type) in &proj.package.overrides {
            extra.push_str(&format!(
                "<Override PartName=\"{}\" ContentType=\"{}\"/>",
                escape_attr(part_name),
                escape_attr(content_type)
            ));
        }
        content_types.insert_str(insertion, &extra);
    }
    let mut entries = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types.into_bytes(),
        ),
        (MAIN_PART.to_string(), write_mspdi(proj).into_bytes()),
    ];
    entries.extend(
        proj.package
            .parts
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.to_vec())),
    );
    write_zip(&entries)
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
                let name = parser.name().to_string();
                if stack.is_empty() && (root_closed || name != "Types") {
                    return None;
                }
                if stack.len() == 1 && name == "Default" {
                    let ext = decoded(parser.attr("Extension"));
                    let kind = decoded(parser.attr("ContentType"));
                    if !ext.is_empty() && !kind.is_empty() {
                        if ext.eq_ignore_ascii_case("xml") {
                            xml_default = Some(kind);
                        } else {
                            defaults.push((ext, kind));
                        }
                    }
                } else if stack.len() == 1 && name == "Override" {
                    let part = decoded(parser.attr("PartName"));
                    let kind = decoded(parser.attr("ContentType"));
                    if !part.is_empty() && !kind.is_empty() {
                        overrides.push((part, kind));
                    }
                }
                stack.push(name);
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
            Event::Text => {}
        }
    }
    if !root_closed || !stack.is_empty() {
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
        if kind != "application/vnd.yppx.project+xml" {
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

/// Read a `.yppx` package back into a [`Project`].
pub fn read_yppx(bytes: &[u8]) -> Result<Project, String> {
    let zip = ZipArchive::open(bytes).ok_or("not a valid .yppx (ZIP) container")?;
    let part = zip
        .read(MAIN_PART)
        .ok_or_else(|| format!(".yppx package is missing its {MAIN_PART} part"))?;
    let xml = String::from_utf8(part).map_err(|_| format!("{MAIN_PART} is not valid UTF-8"))?;
    let mut project = read_mspdi(&xml)?;
    for entry in zip.entries() {
        let name = &entry.name;
        if name.ends_with('/')
            || name.eq_ignore_ascii_case("[Content_Types].xml")
            || name.eq_ignore_ascii_case(MAIN_PART)
            || project
                .package
                .parts
                .iter()
                .any(|(kept, _)| kept.eq_ignore_ascii_case(name))
        {
            continue;
        }
        let bytes = zip
            .extract(entry)
            .ok_or_else(|| format!("cannot read .yppx part {name}"))?;
        project.package.parts.push((name.clone(), Arc::from(bytes)));
    }
    if let Some(map) = zip.read("[Content_Types].xml") {
        if let Ok(map) = std::str::from_utf8(&map) {
            read_content_types(map, &mut project.package);
        }
    }
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datetime::DateTime;
    use crate::model::*;

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
        let bytes = write_yppx(&Project::default());
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
        let bytes = write_yppx(&sample());
        assert_eq!(&bytes[..2], b"PK"); // ZIP local-file-header magic
        // and the container advertises the two expected parts
        let zip = ZipArchive::open(&bytes).unwrap();
        assert!(zip.read("[Content_Types].xml").is_some());
        assert!(zip.read(MAIN_PART).is_some());
        assert_eq!(zip.entries().len(), 2);
        assert_eq!(
            zip.read("[Content_Types].xml").unwrap(),
            CONTENT_TYPES.as_bytes()
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
        let output = write_yppx(&project);
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
        let output = write_yppx(&project);
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
        let output = write_yppx(&read_yppx(&source).unwrap());
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
        let output = write_yppx(&read_yppx(&source).unwrap());
        let zip = ZipArchive::open(&output).unwrap();
        assert_eq!(zip.read("views.xml").unwrap(), b"<views/>");
        assert_eq!(
            zip.read("[Content_Types].xml").unwrap(),
            CONTENT_TYPES.as_bytes()
        );
    }

    #[test]
    fn round_trips_through_package() {
        let orig = sample();
        let bytes = write_yppx(&orig);
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
                let result = read_yppx(&write_yppx(&project)).unwrap();
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
        let error = read_yppx(&write_yppx(&proj)).unwrap_err();
        assert!(error.contains("calendar \"Standard\" (UID 1) has no working time"));
        assert!(error.contains("task \"Design & build\" (UID 1) cannot be scheduled"));
    }
}
