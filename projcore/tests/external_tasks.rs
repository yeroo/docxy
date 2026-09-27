use projcore::DateTime;
use projcore::editor::{format_predecessors, parse_task_predecessors};
use projcore::gantt::to_markdown;
use projcore::mspdi::{read_mspdi, write_mspdi};
use projcore::schedule::{level, schedule};
use projcore::yppx::{read_yppx, write_yppx};

fn fixture(path: &str, start_finish: &str) -> String {
    let xml = include_str!("../../corpus/mspdi/02-link-fs.xml");
    let link = format!(
        "<PredecessorLink><PredecessorUID>3</PredecessorUID><Type>1</Type><CrossProject>1</CrossProject><CrossProjectName>{path}\\7</CrossProjectName></PredecessorLink>"
    );
    let task = format!(
        "<Task><UID>3</UID><ID>3</ID><Name>Deliver</Name><OutlineLevel>1</OutlineLevel><Duration>PT8H0M0S</Duration><DurationFormat>7</DurationFormat>{start_finish}<ExternalTask>1</ExternalTask><ExternalTaskProject>{path}</ExternalTaskProject></Task>"
    );
    xml.replacen(
        "      </PredecessorLink>",
        &format!("      </PredecessorLink>{link}"),
        1,
    )
    .replacen("  </Tasks>", &format!("{task}  </Tasks>"), 1)
}

fn at(day: u32, hour: u32) -> DateTime {
    DateTime::from_ymd_hm(2026, 3, day, hour, 0)
}

#[test]
fn cross_project_fields_survive_xml_yppx_and_cell_edit() {
    let path = r"C:\plans\other&amp;more.mpp";
    let proj = read_mspdi(&fixture(path, "")).unwrap();
    let external = proj.task(3).unwrap();
    assert_eq!(
        external.external_task_project.as_deref(),
        Some(r"C:\plans\other&more.mpp")
    );
    let link = &proj.task(2).unwrap().predecessors[1];
    assert_eq!(link.cross_project, Some(true));
    assert_eq!(
        link.cross_project_name.as_deref(),
        Some(r"C:\plans\other&more.mpp\7")
    );

    let shown = format_predecessors(proj.task(2).unwrap(), &proj);
    let parsed = parse_task_predecessors(&shown, proj.task(2).unwrap(), &proj).unwrap();
    assert_eq!(parsed[1], *link);
    let with_new = parse_task_predecessors(&format!("{shown}, 3SS"), proj.task(2).unwrap(), &proj);
    assert!(with_new.is_err()); // Duplicate UID is still rejected.

    let saved = write_mspdi(&proj);
    assert!(
        saved.contains(r"<ExternalTaskProject>C:\plans\other&amp;more.mpp</ExternalTaskProject>")
    );
    assert!(saved.contains(r"<CrossProjectName>C:\plans\other&amp;more.mpp\7</CrossProjectName>"));
    let a = saved.find("<ExternalTask>1</ExternalTask>").unwrap();
    let b = saved.find("<ExternalTaskProject>").unwrap();
    assert!(a < b);
    let l = saved.find("<CrossProject>1</CrossProject>").unwrap();
    let n = saved.find("<CrossProjectName>").unwrap();
    let lag = saved[l..].find("<LinkLag>").unwrap() + l;
    assert!(l < n && n < lag);
    for back in [
        read_mspdi(&saved).unwrap(),
        read_yppx(&write_yppx(&proj)).unwrap(),
    ] {
        assert_eq!(
            back.task(3).unwrap().external_task_project,
            external.external_task_project
        );
        assert_eq!(back.task(2).unwrap().predecessors[1], *link);
    }

    let plain =
        write_mspdi(&read_mspdi(include_str!("../../corpus/mspdi/02-link-fs.xml")).unwrap());
    assert!(!plain.contains("CrossProject"));
    assert!(!plain.contains("ExternalTaskProject"));

    let mut false_link = proj.clone();
    false_link
        .tasks
        .iter_mut()
        .find(|t| t.uid == 2)
        .unwrap()
        .predecessors[1]
        .cross_project = Some(false);
    let false_saved = write_mspdi(&false_link);
    assert!(false_saved.contains("<CrossProject>0</CrossProject>"));
    assert_eq!(
        read_mspdi(&false_saved)
            .unwrap()
            .task(2)
            .unwrap()
            .predecessors[1]
            .cross_project,
        Some(false)
    );
}

#[test]
fn external_leaf_is_not_local_work_or_a_project_bound() {
    let proj = read_mspdi(&fixture(r"C:\plans\other.mpp", "")).unwrap();
    let sched = schedule(&proj);
    assert!(sched.get(3).is_none());
    assert_eq!(sched.get(2).unwrap().early_start, at(4, 8));
    assert_eq!(sched.project_finish, at(5, 17));
    assert!(sched.get(1).unwrap().critical && sched.get(2).unwrap().critical);
    assert!(level(&proj).start(3).is_none());
    assert_eq!(level(&proj).project_finish, at(5, 17));
    let md = to_markdown(&proj, &sched);
    assert!(!md.contains("Deliver"));
    assert!(md.contains("2026-03-04"));

    let mut dated = proj.clone();
    dated
        .tasks
        .iter_mut()
        .find(|t| t.uid == 3)
        .unwrap()
        .stored_start = Some(at(9, 8));
    dated
        .tasks
        .iter_mut()
        .find(|t| t.uid == 3)
        .unwrap()
        .stored_finish = Some(at(9, 17));
    let sched = schedule(&dated);
    let external = sched.get(3).unwrap();
    assert_eq!(
        (external.early_start, external.early_finish),
        (at(9, 8), at(9, 17))
    );
    assert!(!external.critical);
    assert_eq!(sched.get(2).unwrap().early_start, at(4, 8));
    assert_eq!(sched.project_finish, at(5, 17));
    assert_eq!(level(&dated).start(3), Some(at(9, 8)));
    assert_eq!(level(&dated).project_finish, at(5, 17));
    let saved = write_mspdi(&dated);
    let ext_xml = &saved[saved.find("<UID>3</UID>").unwrap()..];
    assert!(ext_xml.contains("<Start>2026-03-09T08:00:00</Start>"));
    assert!(ext_xml.contains("<Finish>2026-03-09T17:00:00</Finish>"));
    assert!(!ext_xml.contains("<Critical>"));
    assert!(!ext_xml.contains("<TotalSlack>"));

    let before = DateTime::from_ymd_hm(2026, 2, 23, 8, 0);
    dated
        .tasks
        .iter_mut()
        .find(|t| t.uid == 3)
        .unwrap()
        .stored_start = Some(before);
    dated
        .tasks
        .iter_mut()
        .find(|t| t.uid == 3)
        .unwrap()
        .stored_finish = Some(before);
    let early = schedule(&dated);
    assert_eq!(early.project_start, at(2, 8));
    assert_eq!(early.project_finish, at(5, 17));
    assert_eq!(early.get(3).unwrap().early_start, before);

    dated.tasks.retain(|t| t.uid == 3);
    assert_eq!(schedule(&dated).project_start, at(2, 8));
    assert_eq!(schedule(&dated).project_finish, at(2, 8));
    assert_eq!(level(&dated).project_finish, at(2, 8));
    let summary = projcore::Task {
        uid: 4,
        id: 4,
        name: "External group".into(),
        outline_level: 1,
        summary: true,
        ..Default::default()
    };
    dated.tasks[0].outline_level = 2;
    dated.tasks.insert(0, summary);
    assert!(schedule(&dated).rolled_up(4).is_none());
    assert!(level(&dated).rolled_up(4).is_none());
    dated.start_date = None;
    let fallback = DateTime::from_ymd_hm(2020, 1, 6, 8, 0);
    assert_eq!(schedule(&dated).project_start, fallback);
    assert_eq!(schedule(&dated).project_finish, fallback);
    assert_eq!(level(&dated).project_finish, fallback);
}
