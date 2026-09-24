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

use crate::model::Project;
use crate::mspdi::{read_mspdi, write_mspdi};
use opccore::zip::ZipArchive;
use opccore::zipwrite::write_zip;
use std::path::{Path, PathBuf};

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
    let entries = vec![
        (
            "[Content_Types].xml".to_string(),
            CONTENT_TYPES.as_bytes().to_vec(),
        ),
        (MAIN_PART.to_string(), write_mspdi(proj).into_bytes()),
    ];
    write_zip(&entries)
}

/// Read a `.yppx` package back into a [`Project`].
pub fn read_yppx(bytes: &[u8]) -> Result<Project, String> {
    let zip = ZipArchive::open(bytes).ok_or("not a valid .yppx (ZIP) container")?;
    let part = zip
        .read(MAIN_PART)
        .ok_or_else(|| format!(".yppx package is missing its {MAIN_PART} part"))?;
    let xml = String::from_utf8(part).map_err(|_| format!("{MAIN_PART} is not valid UTF-8"))?;
    read_mspdi(&xml)
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
        b.predecessors = vec![Predecessor {
            uid: 1,
            link: LinkType::FinishStart,
            lag_min: 240,
        }];
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
