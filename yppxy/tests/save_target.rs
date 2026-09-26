use std::path::Path;
use std::process::Command;

#[test]
fn headless_save_adds_native_extension_and_refuses_mpp() {
    let dir = std::env::temp_dir().join(format!("yppxy-cli-save-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("plan");
    let result = Command::new(env!("CARGO_BIN_EXE_yppxy"))
        .arg("--save")
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let actual = target.with_extension("yppx");
    assert!(projcore::yppx::read_yppx(&std::fs::read(&actual).unwrap()).is_ok());
    assert!(!target.exists());
    let binary = dir.join("plan.mpp");
    std::fs::write(&binary, b"original binary schedule").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_yppxy"))
        .arg("--save")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("Project schedules can only be saved")
    );
    assert_eq!(std::fs::read(&binary).unwrap(), b"original binary schedule");
    std::fs::remove_file(actual).unwrap();
    std::fs::remove_file(binary).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

const SAVE_FORMAT_ERROR: &str = "Project schedules can only be saved as .yppx or .xml (MSPDI)";

fn yppxy_save(input: Option<&Path>, target: &Path) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_yppxy"));
    if let Some(input) = input {
        cmd.arg(input);
    }
    cmd.arg("--save").arg(target).output().unwrap()
}

/// Issue #78: `--save` once wrote MSPDI XML under any extension and exited 0.
/// Every row of the issue's table must either write the named format or refuse
/// without creating the file.
#[test]
fn headless_save_never_writes_one_format_under_another_name() {
    let dir = std::env::temp_dir().join(format!("yppxy-cli-save-issue78-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Distinct stems: the file system may be case-insensitive.
    for name in [
        "out.mpp",
        "out.mpt",
        "out.xlsx",
        "out.csv",
        "out.pdf",
        "upper.MPP",
    ] {
        let target = dir.join(name);
        let result = yppxy_save(None, &target);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{name}: expected refusal");
        assert!(stderr.contains(SAVE_FORMAT_ERROR), "{name}: {stderr}");
        assert!(!target.exists(), "{name}: refused save created the file");
    }

    // The issue's exact shape: an MSPDI input converted to a refused target.
    let input = dir.join("plan.xml");
    let project = projcore::editor::untitled_project();
    std::fs::write(&input, projcore::mspdi::write_mspdi(&project)).unwrap();
    let target = dir.join("converted.mpp");
    let result = yppxy_save(Some(&input), &target);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains(SAVE_FORMAT_ERROR));
    assert!(!target.exists());

    let target = dir.join("out.yppx");
    let result = yppxy_save(None, &target);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = std::fs::read(&target).unwrap();
    assert!(bytes.starts_with(b"PK"), "out.yppx is not a ZIP package");
    assert!(projcore::yppx::read_yppx(&bytes).is_ok());

    for name in ["out.xml", "upper.XML"] {
        let target = dir.join(name);
        let result = yppxy_save(None, &target);
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let text = String::from_utf8(std::fs::read(&target).unwrap()).unwrap();
        assert!(text.starts_with('<'), "{name} is not XML");
        assert!(
            projcore::mspdi::read_mspdi(&text).is_ok(),
            "{name} is not MSPDI"
        );
    }

    std::fs::remove_dir_all(dir).unwrap();
}

/// `--gantt-md` used to win and exit 0 without ever attempting the `--save`.
#[test]
fn headless_gantt_md_and_save_together_are_refused() {
    let dir = std::env::temp_dir().join(format!("yppxy-cli-save-combo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let gantt = dir.join("g.md");
    let target = dir.join("out.yppx");
    let result = Command::new(env!("CARGO_BIN_EXE_yppxy"))
        .arg("--gantt-md")
        .arg(&gantt)
        .arg("--save")
        .arg(&target)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("--gantt-md and --save cannot be combined"),
        "{stderr}"
    );
    assert!(!gantt.exists());
    assert!(!target.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

/// Issue #82: `yppxy plan.xml --save out.xml` reset every project option docxy
/// does not model. The schedule direction, currency and task defaults a plan's
/// owner chose must reach the saved file, in both formats.
#[test]
fn headless_save_keeps_project_options() {
    let dir = std::env::temp_dir().join(format!("yppxy-cli-save-issue82-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("plan.xml");
    std::fs::write(
        &input,
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Project xmlns="http://schemas.microsoft.com/project">
  <Name>plan.xml</Name>
  <ScheduleFromStart>0</ScheduleFromStart>
  <StartDate>2026-03-02T08:00:00</StartDate>
  <FinishDate>2026-03-06T17:00:00</FinishDate>
  <CurrencyCode>EUR</CurrencyCode>
  <CalendarUID>1</CalendarUID>
  <MinutesPerDay>480</MinutesPerDay>
  <DefaultTaskType>1</DefaultTaskType>
  <Tasks>
    <Task><UID>1</UID><ID>1</ID><Name>Build</Name><Duration>PT40H0M0S</Duration></Task>
  </Tasks>
</Project>
"#,
    )
    .unwrap();
    for name in ["out.xml", "out.yppx"] {
        let target = dir.join(name);
        let result = yppxy_save(Some(&input), &target);
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let bytes = std::fs::read(&target).unwrap();
        // The .yppx package wraps the same MSPDI part; read it to reach it.
        let xml = if name.ends_with(".xml") {
            String::from_utf8(bytes).unwrap()
        } else {
            projcore::mspdi::write_mspdi(&projcore::yppx::read_yppx(&bytes).unwrap())
        };
        for option in [
            "<ScheduleFromStart>0</ScheduleFromStart>",
            "<FinishDate>2026-03-06T17:00:00</FinishDate>",
            "<CurrencyCode>EUR</CurrencyCode>",
            "<DefaultTaskType>1</DefaultTaskType>",
        ] {
            assert!(xml.contains(option), "{name}: lost {option}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Issue #84: saving dropped each rate's display unit, booking type, resource flags and
/// assignment contours, so a daily rate could reopen read as hourly.
#[test]
fn headless_save_keeps_resource_and_assignment_fields() {
    let dir = std::env::temp_dir().join(format!("yppxy-cli-save-issue84-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mspdi/13-resource-fields.xml");
    let source = std::fs::read_to_string(&input).unwrap();
    let section = |xml: &str, name: &str| {
        let open = xml.find(&format!("<{name}>")).unwrap();
        xml[open..xml.find(&format!("</{name}>")).unwrap()].to_string()
    };
    for name in ["out.xml", "out.yppx"] {
        let target = dir.join(name);
        let result = yppxy_save(Some(&input), &target);
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let bytes = std::fs::read(&target).unwrap();
        // The .yppx package wraps the same MSPDI part; read it to reach it.
        let xml = if name.ends_with(".xml") {
            String::from_utf8(bytes).unwrap()
        } else {
            projcore::mspdi::write_mspdi(&projcore::yppx::read_yppx(&bytes).unwrap())
        };
        for (section_name, elements) in [
            (
                "Resources",
                &[
                    "<StandardRateFormat>3</StandardRateFormat>",
                    "<OvertimeRateFormat>4</OvertimeRateFormat>",
                    "<BookingType>1</BookingType>",
                    "<IsGeneric>1</IsGeneric>",
                    "<IsBudget>1</IsBudget>",
                    "<IsInactive>0</IsInactive>",
                    "<IsInactive>1</IsInactive>",
                    "<CanLevel>1</CanLevel>",
                    "<WorkGroup>1</WorkGroup>",
                    "<PeakUnits>1</PeakUnits>",
                    "<OverAllocated>0</OverAllocated>",
                    "<Work>PT16H0M0S</Work>",
                    "<RegularWork>PT16H0M0S</RegularWork>",
                    "<RemainingWork>PT16H0M0S</RemainingWork>",
                ][..],
            ),
            (
                "Assignments",
                &[
                    "<WorkContour>0</WorkContour>",
                    "<FixedMaterial>0</FixedMaterial>",
                    "<HasFixedRateUnits>1</HasFixedRateUnits>",
                    "<Start>2026-03-02T08:00:00</Start>",
                    "<Finish>2026-03-03T17:00:00</Finish>",
                    "<RegularWork>PT16H0M0S</RegularWork>",
                    "<RemainingWork>PT16H0M0S</RemainingWork>",
                    "<PercentWorkComplete>0</PercentWorkComplete>",
                ][..],
            ),
        ] {
            assert!(section(&source, section_name).contains(elements[0]));
            let saved = section(&xml, section_name);
            for element in elements {
                assert!(saved.contains(element), "{name}: lost {element}");
            }
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// The `<Name>value</Name>` leaves carrying a value, in document order.
fn valued_leaves(xml: &str) -> Vec<&str> {
    let mut leaves = Vec::new();
    let mut rest = xml;
    while let Some(open) = rest.find('<') {
        rest = &rest[open..];
        let Some(end) = rest.find('>') else { break };
        let name = &rest[1..end];
        let close = format!("</{name}>");
        let body = &rest[end + 1..];
        match body.find('<') {
            Some(next) if !name.starts_with('/') && body[next..].starts_with(&close) => {
                if next > 0 {
                    leaves.push(&rest[..end + 1 + next + close.len()]);
                }
                rest = &body[next + close.len()..];
            }
            _ => rest = body,
        }
    }
    leaves
}

/// Issue #199, by its own method: save file 13 with `--save` and list the
/// resource and assignment elements carrying a value in the input that are
/// missing from the output. Rate tables, availability periods, delays,
/// overtime, costs, notes, custom fields, resource baselines and timephased
/// data were all on that list. Issue #267 extended file 13 to every other
/// Resource and Assignment child (GUIDs, hyperlinks, earned value, outline
/// codes, resource timephased data, budget, ...).
#[test]
fn headless_save_keeps_every_valued_resource_and_assignment_element() {
    let dir = std::env::temp_dir().join(format!("yppxy-cli-save-issue199-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mspdi/13-resource-fields.xml");
    let source = std::fs::read_to_string(&input).unwrap();
    let section = |xml: &str, name: &str| {
        let open = xml.find(&format!("<{name}>")).unwrap();
        xml[open..xml.find(&format!("</{name}>")).unwrap()].to_string()
    };
    for name in ["out.xml", "out.yppx"] {
        let target = dir.join(name);
        let result = yppxy_save(Some(&input), &target);
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let bytes = std::fs::read(&target).unwrap();
        let xml = if name.ends_with(".xml") {
            String::from_utf8(bytes).unwrap()
        } else {
            projcore::mspdi::write_mspdi(&projcore::yppx::read_yppx(&bytes).unwrap())
        };
        for (section_name, samples) in [
            (
                "Resources",
                [
                    "<RateTable>1</RateTable>",
                    "<AvailableUnits>1</AvailableUnits>",
                    // #267: a nested outline code and timephased Baseline
                    // cost, and a plain scalar.
                    "<ValueID>3</ValueID>",
                    "<Value>485</Value>",
                    "<GUID>0B7E4C2A-1D3F-4E5A-8B6C-7D8E9F0A1B2C</GUID>",
                ],
            ),
            (
                "Assignments",
                [
                    "<CostRateTable>1</CostRateTable>",
                    "<Value>PT8H0M0S</Value>",
                    "<RateScale>2</RateScale>",
                    "<BudgetWork>PT18H0M0S</BudgetWork>",
                    "<GUID>5D6E7F80-9A1B-4C2D-8E3F-405162738495</GUID>",
                ],
            ),
        ] {
            let (before, after) = (section(&source, section_name), section(&xml, section_name));
            let leaves = valued_leaves(&before);
            // Guard the scan itself: it must see leaves nested in blocks.
            for sample in samples {
                assert!(leaves.contains(&sample), "scan missed {sample}");
            }
            let missing: Vec<_> = leaves
                .iter()
                .filter(|leaf| after.matches(*leaf).count() < before.matches(*leaf).count())
                .collect();
            assert!(missing.is_empty(), "{name} {section_name} lost {missing:?}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
