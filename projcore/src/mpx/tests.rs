use super::*;
use crate::model::LinkType;

fn corpus(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../corpus/mpx")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

const HEADER: &str = "MPX,Microsoft Project for Windows,4.0,ANSI\r\n";

/// A file of `body` records after the header.
fn mpx(body: &str) -> Vec<u8> {
    format!("{HEADER}{body}").into_bytes()
}

fn at(year: i64, month: u32, day: u32, hour: u32, minute: u32) -> DateTime {
    DateTime::from_ymd_hm(year, month, day, hour, minute)
}

#[test]
fn records_split_on_the_delimiter_outside_quotes() {
    let text = "10,$,1,2,\",\",.\r\n\r\n70,\"a, \"\"b\"\"\nc\",2\n71,last";
    assert_eq!(
        records(text, ','),
        [
            vec!["10", "$", "1", "2", ",", "."],
            vec!["70", "a, \"b\"\nc", "2"],
            vec!["71", "last"],
        ]
    );
    assert_eq!(records("70;a,b;\"c;d\"", ';'), [vec!["70", "a,b", "c;d"]]);
    // An unterminated quote runs to the end; nothing is lost or panics.
    assert_eq!(records("70,\"abc", ','), [vec!["70", "abc"]]);
}

#[test]
fn bytes_decode_as_utf8_else_windows_1252() {
    assert_eq!(decode("\u{FEFF}Café".as_bytes()), "Café");
    assert_eq!(
        decode(b"Caf\xe9 \x80\x81\x8d\x8f\x90\x9d\x9f"),
        "Café €\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}Ÿ"
    );
    let project = read_mpx(
        &[
            HEADER.as_bytes(),
            b"60,Name,ID,Unique ID\r\n61,1,90,98\r\n70,Caf\xe9 \x93x\x94,1,1\r\n",
        ]
        .concat(),
    )
    .unwrap();
    assert_eq!(project.tasks[0].name, "Café \u{201C}x\u{201D}");
}

#[test]
fn dates_follow_the_date_order_and_two_digit_years_pivot_at_80() {
    let mut s = Settings::default();
    assert_eq!(s.date("03/04/2026"), Some((at(2026, 3, 4, 0, 0), None)));
    assert_eq!(s.date("03/04/26"), Some((at(2026, 3, 4, 0, 0), None)));
    assert_eq!(s.date("03/04/79"), Some((at(2079, 3, 4, 0, 0), None)));
    assert_eq!(s.date("03/04/80"), Some((at(1980, 3, 4, 0, 0), None)));
    assert_eq!(
        s.date("03/04/2026 01:30 pm"),
        Some((at(2026, 3, 4, 0, 0), Some(13 * 60 + 30)))
    );
    s.date_order = DateOrder::Dmy;
    assert_eq!(s.date("04/03/26"), Some((at(2026, 3, 4, 0, 0), None)));
    s.date_order = DateOrder::Ymd;
    assert_eq!(s.date("26/03/04"), Some((at(2026, 3, 4, 0, 0), None)));
    assert_eq!(s.date("2026/03/04"), Some((at(2026, 3, 4, 0, 0), None)));
    for bad in [
        "",
        "2026/02/30",
        "2026/13/01",
        "2026/3",
        "x/1/2",
        "2026/03/04 25:00",
        "1/2/3/4",
    ] {
        assert_eq!(s.date(bad), None, "{bad}");
    }
}

#[test]
fn times_read_am_pm_and_24_hours() {
    let s = Settings::default();
    for (text, minutes) in [
        ("08:00 am", 480),
        ("12:00 am", 0),
        ("12:00 pm", 720),
        ("01:00 pm", 780),
        ("05:30 PM", 17 * 60 + 30),
        ("13:00", 780),
        ("24:00", 1440),
    ] {
        assert_eq!(s.time(text), Some(minutes), "{text}");
    }
    for bad in ["13:00 pm", "00:00 am", "08:60 am", "x", "", "25:00"] {
        assert_eq!(s.time(bad), None, "{bad}");
    }
}

#[test]
fn money_reads_the_currency_and_separators_of_record_10() {
    let mut s = Settings::default();
    assert_eq!(s.money("$1,250.50").unwrap().to_f64(), Some(1250.5));
    assert_eq!(s.money("-$0.12").unwrap().to_f64(), Some(-0.12));
    assert_eq!(s.money("($0.12)").unwrap().to_f64(), Some(-0.12));
    s.currency = "€".into();
    s.thousands = '.';
    s.decimal = ',';
    assert_eq!(s.money("1.250,50 €").unwrap().to_f64(), Some(1250.5));
    assert_eq!(s.money("€"), None);

    // Record 10 sets them, a space thousands separator included.
    let project = read_mpx(&mpx(
        "10,€,1,2, ,\",\"\r\n40,Name,Standard Rate,ID,Unique ID\r\n41,1,42,40,49\r\n\
         50,Ann,\"1 234,50 €/h\",1,1\r\n61,1,90,98\r\n70,A,1,1\r\n",
    ))
    .unwrap();
    assert_eq!(
        project.resources[0]
            .standard_rate
            .as_ref()
            .unwrap()
            .to_f64(),
        Some(1234.5)
    );
}

#[test]
fn a_file_without_the_header_or_without_tasks_is_refused() {
    for bytes in [
        b"".as_slice(),
        b"MPX",
        b"60,Name\r\n70,A",
        b"<Project/>",
        b"MPXJ,1",
    ] {
        assert!(read_mpx(bytes).unwrap_err().contains("MPX"), "{bytes:?}");
    }
    assert_eq!(
        read_mpx(&mpx("10,$,1,2,\",\",.\r\n30,Plan\r\n")).unwrap_err(),
        "not an MPX project: no task records"
    );
}

#[test]
fn header_sniff_takes_a_comma_or_semicolon_after_an_optional_bom() {
    assert!(is_mpx(b"MPX,Microsoft Project"));
    assert!(is_mpx(b"MPX;x"));
    assert!(is_mpx(b"\xEF\xBB\xBFMPX,"));
    for bytes in [b"MPX".as_slice(), b"MPX\t", b"mpx,", b"MPXJ", b" MPX,", b""] {
        assert!(!is_mpx(bytes), "{bytes:?}");
    }
}

#[test]
fn unknown_records_and_field_ids_are_ignored() {
    let project = read_mpx(&mpx("99,anything,at,all\r\n\
         x,not a record\r\n\
         60,Name,Mystery,ID,Unique ID\r\n61,1,999,90,98\r\n\
         70,A,whatever,1,1\r\n\
         72,a recurring task record\r\n\
         75,9,1,8h\r\n\
         76,,0,0,NA,NA\r\n"))
    .unwrap();
    assert_eq!(project.tasks.len(), 1);
    assert_eq!(project.tasks[0].name, "A");
    // The assignment names resource 9, which the file does not have.
    assert!(project.assignments.is_empty());
}

#[test]
fn tasks_order_by_id_and_a_nameless_levelless_row_is_blank() {
    let project = read_mpx(&corpus("20-task-fields.mpx")).unwrap();
    let rows: Vec<(i32, i32, bool, bool)> = project
        .tasks
        .iter()
        .map(|t| (t.id, t.uid, t.is_null, t.summary))
        .collect();
    assert_eq!(
        rows,
        [
            (1, 1, false, true),
            (2, 2, false, false),
            (3, 3, true, false),
            (4, 4, false, false),
            (5, 5, false, false),
        ]
    );
    let phase = &project.tasks[0];
    assert_eq!(phase.task_type, Some(TaskType::FixedDuration));
    assert_eq!(phase.priority, Some(500));
    let excavate = &project.tasks[1];
    assert_eq!(excavate.priority, Some(900));
    assert_eq!(excavate.task_type, None);
    assert_eq!(excavate.outline_level, 2);
    assert_eq!(excavate.wbs.as_deref(), Some("1.1"));
    let pour = &project.tasks[3];
    // Delay 1d: MSPDI's LevelingDelay in tenths of a minute, format days.
    assert_eq!(pour.leveling_delay, Some(4800));
    assert_eq!(pour.leveling_delay_format, Some(7));
    assert_eq!(pour.contact.as_deref(), Some("Site lead"));
    assert_eq!(pour.hide_bar, Some(true));
    // "2,3": the link to the blank row is kept, as the MSPDI twin has it.
    assert_eq!(
        pour.predecessors.iter().map(|p| p.uid).collect::<Vec<_>>(),
        [2, 3]
    );
}

#[test]
fn rows_without_an_id_keep_their_place_and_duplicate_uids_are_renumbered() {
    let project = read_mpx(&mpx("60,Name,ID,Unique ID\r\n61,1,90,98\r\n\
         70,C,3,7\r\n70,A,1,7\r\n70,NoId,,\r\n70,B,2,1\r\n"))
    .unwrap();
    let rows: Vec<(&str, i32, i32)> = project
        .tasks
        .iter()
        .map(|t| (t.name.as_str(), t.id, t.uid))
        .collect();
    // NoId follows A, the row before it; the second UID 7 takes a fresh one.
    assert_eq!(
        rows,
        [("A", 1, 8), ("NoId", 2, 9), ("B", 2, 1), ("C", 3, 7)]
    );
}

#[test]
fn predecessors_name_task_ids_with_type_and_lag() {
    let project = read_mpx(&corpus("26-lag-percent-elapsed.mpx")).unwrap();
    let link = |name: &str| {
        let task = project.tasks.iter().find(|t| t.name == name).unwrap();
        let p = &task.predecessors[0];
        (p.uid, p.link, p.lag, p.lag_format.code())
    };
    assert_eq!(link("B"), (1, LinkType::FinishStart, 50, 19));
    assert_eq!(link("C"), (1, LinkType::FinishStart, -25, 19));
    assert_eq!(link("D"), (1, LinkType::FinishStart, 2 * 1440, 8));
    assert_eq!(link("F"), (1, LinkType::StartStart, 5 * 1440, 8));
    assert_eq!(link("G"), (1, LinkType::FinishFinish, 2 * 1440, 8));
    assert_eq!(link("H"), (1, LinkType::FinishStart, 7 * 1440, 10));
    assert_eq!(link("S"), (1, LinkType::StartFinish, 2 * 1440, 8));
    assert_eq!(link("W"), (1, LinkType::FinishStart, 180, 5));
    assert_eq!(link("X"), (9, LinkType::StartStart, 150, 19));

    let project = read_mpx(&mpx(
        "60,Name,Predecessors,ID,Unique ID\r\n61,1,70,90,98\r\n\
         70,A,,1,10\r\n70,B,,2,20\r\n\
         70,C,\"1FS+2d, 2SS-1d, 9, 3, 1\",3,30\r\n",
    ))
    .unwrap();
    let links: Vec<(i32, LinkType, i64)> = project.tasks[2]
        .predecessors
        .iter()
        .map(|p| (p.uid, p.link, p.lag))
        .collect();
    // ID 9 names no task, 3 is the task itself and the second 1 repeats a
    // link: each drops alone.
    assert_eq!(
        links,
        [
            (10, LinkType::FinishStart, 960),
            (20, LinkType::StartStart, -480)
        ]
    );
}

#[test]
fn record_11_hours_convert_days_and_weeks() {
    let project = read_mpx(&mpx("11,1,0,1,7.50,30.00,,,0,0\r\n\
         60,Name,Duration,ID,Unique ID\r\n61,1,40,90,98\r\n\
         70,A,2d,1,1\r\n70,B,1w,2,2\r\n70,C,3h?,3,3\r\n70,D,2ed,4,4\r\n70,E,1.5,5,5\r\n"))
    .unwrap();
    assert_eq!(project.hours_per_day, 7.5);
    assert_eq!(project.hours_per_week, 30.0);
    let durations: Vec<(i64, Option<u8>, Option<bool>)> = project
        .tasks
        .iter()
        .map(|t| (t.duration_min, t.duration_format, t.estimated))
        .collect();
    assert_eq!(
        durations,
        [
            (900, None, None),
            (1800, Some(9), None),
            (180, Some(37), Some(true)),
            (2880, Some(8), None),
            (675, None, None),
        ]
    );
}

#[test]
fn whole_day_dates_land_on_the_calendar_day_boundaries() {
    let project = read_mpx(&corpus("22-progress.mpx")).unwrap();
    assert_eq!(project.start_date, Some(at(2026, 3, 2, 8, 0)));
    let excavate = &project.tasks[0];
    assert_eq!(excavate.actual_start, Some(at(2026, 3, 2, 8, 0)));
    assert_eq!(excavate.actual_finish, Some(at(2026, 3, 3, 17, 0)));
    assert_eq!(excavate.percent_complete, Some(100));
    // A resume on the stop's day is the stop: nothing is left to resume.
    assert_eq!(excavate.stop, Some(at(2026, 3, 3, 17, 0)));
    assert_eq!(excavate.resume, Some(at(2026, 3, 3, 17, 0)));
    let pour = &project.tasks[1];
    assert_eq!(pour.stop, Some(at(2026, 3, 5, 17, 0)));
    assert_eq!(pour.resume, Some(at(2026, 3, 6, 8, 0)));
    assert_eq!(pour.actual_finish, None);
    assert_eq!(pour.stored_start, Some(at(2026, 3, 4, 8, 0)));
    assert_eq!(pour.stored_finish, Some(at(2026, 3, 9, 17, 0)));

    let project = read_mpx(&corpus("30-mfo-honor-constraints-on.mpx")).unwrap();
    assert_eq!(project.tasks[1].constraint, ConstraintType::MustFinishOn);
    assert_eq!(
        project.tasks[1].constraint_date,
        Some(at(2026, 3, 6, 17, 0))
    );
    let project = read_mpx(&corpus("09-constraint-snet.mpx")).unwrap();
    let snet = project
        .tasks
        .iter()
        .find(|t| t.constraint == ConstraintType::StartNoEarlierThan)
        .unwrap();
    assert_eq!(snet.constraint_date, Some(at(2026, 3, 5, 8, 0)));
    // An ASAP task keeps no constraint date, whatever the column holds.
    assert!(
        project
            .tasks
            .iter()
            .filter(|t| t.constraint == ConstraintType::AsSoonAsPossible)
            .all(|t| t.constraint_date.is_none())
    );
}

#[test]
fn base_and_resource_calendars_with_hours_and_exceptions() {
    let project = read_mpx(&corpus("25-derived-calendar-holiday.mpx")).unwrap();
    let standard = &project.calendars[0];
    assert_eq!(standard.name, "Standard");
    assert_eq!(project.default_calendar_uid, standard.uid);
    assert_eq!(standard.base_calendar_uid, None);
    assert_eq!(standard.week[0], Some(DayWorking::default()));
    assert_eq!(standard.week[1].as_ref().unwrap().minutes(), 480);
    let holidays: Vec<(i64, bool)> = standard
        .exceptions
        .iter()
        .map(|e| (e.from.unwrap().day_number(), e.day.working()))
        .collect();
    assert_eq!(
        holidays,
        [
            (at(2026, 3, 4, 0, 0).day_number(), false),
            (at(2026, 3, 11, 0, 0).day_number(), false),
        ]
    );

    let alice = &project.resources[0];
    let calendar = project
        .calendars
        .iter()
        .find(|c| Some(c.uid) == alice.calendar_uid)
        .unwrap();
    assert_eq!(calendar.name, "Alice");
    assert_eq!(calendar.base_calendar_uid, Some(standard.uid));
    // Wednesday 07:00-15:00 of its own; the other days its base's.
    assert_eq!(
        calendar.week[3],
        Some(DayWorking {
            times: vec![WorkingTime { from: 420, to: 900 }]
        })
    );
    assert!(
        calendar
            .week
            .iter()
            .enumerate()
            .all(|(d, w)| d == 3 || w.is_none())
    );
    assert_eq!(calendar.exceptions.len(), 1);
    assert_eq!(
        calendar.exceptions[0].day.times,
        [WorkingTime { from: 420, to: 900 }]
    );
    let resolved = calendar.resolve(|uid| project.calendars.iter().find(|c| c.uid == uid));
    // The base's holiday on 03/04 holds; 03/11 works 07:00-15:00 by Alice's
    // own exception.
    assert!(resolved.day(at(2026, 3, 4, 0, 0).day_number()).is_empty());
    assert_eq!(
        resolved.day(at(2026, 3, 11, 0, 0).day_number()),
        [WorkingTime { from: 420, to: 900 }]
    );

    // A 24-hour day: 12:00 am to 12:00 am.
    let project = read_mpx(&corpus("16-24-hour-calendar.mpx")).unwrap();
    let day = project.calendars[1].week[2].as_ref().unwrap();
    assert_eq!(day.times, [WorkingTime { from: 0, to: 1440 }]);
}

#[test]
fn resources_and_assignments() {
    let project = read_mpx(&corpus("13-resource-fields.mpx")).unwrap();
    let alice = &project.resources[0];
    assert_eq!((alice.uid, alice.id, alice.name.as_str()), (1, 1, "Alice"));
    assert_eq!(alice.max_units, 1.0);
    assert_eq!(alice.initials.as_deref(), Some("A"));
    assert_eq!(alice.group.as_deref(), Some("Eng"));
    assert_eq!(alice.email_address.as_deref(), Some("alice@example.com"));
    // $400.00/d and $3000.00/w are MSPDI's 50 and 75 an hour.
    assert_eq!(alice.standard_rate.as_ref().unwrap().to_f64(), Some(50.0));
    assert_eq!(alice.standard_rate_format, Some(3));
    assert_eq!(alice.overtime_rate.as_ref().unwrap().to_f64(), Some(75.0));
    assert_eq!(alice.overtime_rate_format, Some(4));
    assert_eq!(alice.cost_per_use.as_ref().unwrap().to_f64(), Some(0.1));
    assert_eq!(alice.accrue_at, Some(AccrueAt::Prorated));
    assert_eq!(project.resources.len(), 3);

    let project = read_mpx(&corpus("22-progress.mpx")).unwrap();
    let assignments: Vec<(i32, i32, i32, f64, i64)> = project
        .assignments
        .iter()
        .map(|a| (a.uid, a.task_uid, a.resource_uid, a.units, a.work_min))
        .collect();
    assert_eq!(
        assignments,
        [
            (1, 1, 1, 1.0, 16 * 60),
            (2, 2, 1, 1.0, 32 * 60),
            (3, 3, 1, 1.0, 8 * 60)
        ]
    );
    assert_eq!(project.assignments[1].actual_work_min, Some(16 * 60));
    assert_eq!(project.assignments[1].delay, None);
}

#[test]
fn notes_records_follow_their_row() {
    let project = read_mpx(&mpx(
        "40,Name,ID,Unique ID\r\n41,1,40,49\r\n50,Ann,1,1\r\n51,\"Ann's\r\nnote\"\r\n\
         60,Name,ID,Unique ID\r\n61,1,90,98\r\n70,A,1,1\r\n71,Task note\r\n",
    ))
    .unwrap();
    assert_eq!(project.resources[0].notes.as_deref(), Some("Ann's\nnote"));
    assert_eq!(project.tasks[0].notes.as_deref(), Some("Task note"));
}

#[test]
fn malformed_input_never_panics() {
    let files = [
        "02-link-fs.mpx",
        "13-resource-fields.mpx",
        "20-task-fields.mpx",
        "22-progress.mpx",
        "25-derived-calendar-holiday.mpx",
        "26-lag-percent-elapsed.mpx",
        "28-work-weeks.mpx",
    ];
    for name in files {
        let bytes = corpus(name);
        // Every truncation, read and (when it reads) scheduled.
        for end in 0..bytes.len() {
            if let Ok(project) = read_mpx(&bytes[..end]) {
                let _ = crate::schedule::schedule(&project);
            }
        }
        // Each byte replaced by a delimiter, a quote, a line break and an
        // invalid UTF-8 byte in turn.
        for i in (0..bytes.len()).step_by(3) {
            for b in [b',', b'"', b'\n', 0xff, b'9', b'-'] {
                let mut bad = bytes.clone();
                bad[i] = b;
                let _ = read_mpx(&bad);
            }
        }
    }
    for text in [
        "MPX,\r\n70",
        "MPX,\r\n61,90,98\r\n70,-5,-5\r\n70,99999999999,x",
        "MPX,\r\n20,Cal,9,9,9,9,9,9,9\r\n25,0\r\n25,9,x\r\n26\r\n55\r\n56\r\n57,,\r\n70,A",
        "MPX,\r\n11,1,0,1,0,0\r\n61,40,92,70\r\n70,1e300d,-3d,\"1FS+1e300d,FS,+\"",
        "MPX,\r\n61,1,3\r\n70,A,4294967295\r\n70,B,1\r\n",
        "MPX,\r\n10,,,,,\r\n12,7,7,99999,,,,\r\n30,,,,,99/99/99\r\n61,1\r\n70,A\r\n75,1,-1,-8h",
    ] {
        if let Ok(project) = read_mpx(text.as_bytes()) {
            let _ = crate::schedule::schedule(&project);
        }
    }
}

#[test]
fn a_calendar_without_working_time_is_refused_as_mspdi_refuses_it() {
    let err = read_mpx(&mpx("20,Standard,0,0,0,0,0,0,0\r\n30,P,,,Standard\r\n\
         61,1,90,98\r\n70,A,1,1\r\n"))
    .unwrap_err();
    assert!(err.contains("has no working time"), "{err}");
}
