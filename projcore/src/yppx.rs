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
    fn rejects_non_package() {
        assert!(read_yppx(b"not a zip at all").is_err());
    }
}
