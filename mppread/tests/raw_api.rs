//! Walk the `TBknd*` tables of `.mpp` files through `mppread::raw`, the
//! inspection API, and check what it reads against `project_from_mpp`.
//!
//! The generated snapshots are git-ignored (see corpus/mpp/README.md); the
//! corpus test skips without them. The in-memory tests always run.
use mppread::project::project_from_mpp;
use mppread::raw;
use mppread::{Cfb, Node, write_cfb_tree};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The stream `which` of the `table` storage, wherever the file keeps it.
fn stream(cfb: &Cfb, table: &str, which: &str) -> Option<Vec<u8>> {
    let suffix = format!("/{table}/{which}");
    let path = cfb.paths().into_iter().find(|p| p.ends_with(&suffix))?;
    cfb.read_path(&path)
}

fn snapshots() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/snapshots");
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "mpp"))
        .collect();
    out.sort();
    out
}

/// What one snapshot's walk saw, for the totals printed at the end.
#[derive(Default)]
struct Seen {
    tasks: usize,
    links: usize,
    resources: usize,
    assignments: usize,
    calendars: usize,
}

fn walk(path: &Path, seen: &mut Seen) {
    let at = path.display();
    let bytes = std::fs::read(path).unwrap();
    let cfb = Cfb::open(&bytes).unwrap();
    let project = project_from_mpp(&bytes).unwrap_or_else(|e| panic!("{at}: {e}"));
    let table = raw::decode_task_table(&bytes).unwrap();
    let mpp12 = path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .ends_with("-mpp12");
    assert_eq!(table.legacy, mpp12, "{at}: legacy");

    // TBkndTask: the FixedMeta index gives every decoded row, in ID order.
    let fm = stream(&cfb, "TBkndTask", "FixedMeta").unwrap();
    let fd = stream(&cfb, "TBkndTask", "FixedData").unwrap();
    assert_eq!(
        fm.len(),
        16 + raw::u32_at(&fm, 8) as usize * raw::TASK_META_STRIDE
    );
    let rows = match raw::task_index(&fm, &fd).unwrap() {
        raw::TaskIndex::Current(rows) if !mpp12 => rows,
        raw::TaskIndex::Mpp12(rows) if mpp12 => rows,
        other => panic!("{at}: unexpected index {other:?}"),
    };
    assert_eq!(rows.len(), table.tasks.len(), "{at}: task rows");
    for (row, task) in rows.iter().zip(&table.tasks) {
        assert_eq!(
            (row.id, row.uid, row.is_null),
            (task.id, task.uid, task.is_null),
            "{at}"
        );
        if row.is_null {
            assert_eq!(row.len, 16, "{at}: blank row {}", row.uid);
            continue;
        }
        let rec = &fd[row.offset..row.offset + row.len];
        let (len, start, finish) = if mpp12 {
            assert_eq!(raw::u32_at(rec, 40), task.outline_level.unwrap(), "{at}");
            (raw::MPP12_TASK_RECORD_LEN, 88, 92)
        } else {
            assert_eq!(u32::from(rec[172]), task.outline_level.unwrap(), "{at}");
            (raw::TASK_RECORD_LEN, 0x68, 0x6c)
        };
        assert_eq!(row.len, len, "{at}: record length of UID {}", row.uid);
        assert_eq!(raw::decode_timestamp(rec, start), task.start, "{at}");
        assert_eq!(raw::decode_timestamp(rec, finish), task.finish, "{at}");
    }
    // The import omits UID 0, the project summary row, and keeps the rest.
    let imported: Vec<_> = table
        .tasks
        .iter()
        .filter(|t| t.uid != 0)
        .map(|t| t.uid as i32)
        .collect();
    assert_eq!(
        project.tasks.iter().map(|t| t.uid).collect::<Vec<_>>(),
        imported,
        "{at}: imported tasks"
    );
    seen.tasks += imported.len();

    // Current Project pairs each row with its Fixed2 record.
    if !mpp12 {
        let f2m = stream(&cfb, "TBkndTask", "Fixed2Meta").unwrap();
        let f2d = stream(&cfb, "TBkndTask", "Fixed2Data").unwrap();
        let count = raw::u32_at(&fm, 8) as usize;
        let fixed2 = raw::task_fixed2(&f2m, &f2d, count, &rows).unwrap();
        assert_eq!(fixed2.len(), rows.len());
        for (pair, task) in fixed2.iter().zip(&table.tasks) {
            assert_eq!(pair.meta.len(), raw::FIXED2_META_LEN);
            assert_eq!(pair.data.len(), raw::FIXED2_LEN);
            if task.is_null {
                continue;
            }
            assert_eq!(
                pair.meta[8] & 0x80 != 0,
                task.manual,
                "{at}: UID {}",
                task.uid
            );
        }
    }

    // Task VarMeta: one 0x000e name per live row, the decoded name.
    let vm = stream(&cfb, "TBkndTask", "VarMeta").unwrap();
    let v2 = stream(&cfb, "TBkndTask", "Var2Data").unwrap();
    let entries = raw::var_entries(&vm, &v2).unwrap();
    assert!(entries.iter().all(|e| e.tag == 0x0b40), "{at}: task tags");
    let names: HashMap<u32, String> = entries
        .iter()
        .filter(|e| e.key == 0x000e)
        .map(|e| {
            let name = raw::decode_text(&v2, e.offset, e.uid, "task name", false).unwrap();
            (e.uid, name)
        })
        .collect();
    let live: Vec<_> = table.tasks.iter().filter(|t| !t.is_null).collect();
    assert_eq!(names.len(), live.len(), "{at}: task names");
    for task in &live {
        assert_eq!(names[&task.uid], task.name, "{at}: UID {}", task.uid);
    }

    // TBkndCons: one 20-byte record per predecessor link FixedMeta indexes,
    // on its successor.
    let cons = stream(&cfb, "TBkndCons", "FixedData").unwrap_or_default();
    let cons_meta = stream(&cfb, "TBkndCons", "FixedMeta");
    let records = raw::link_records(cons_meta.as_deref(), &cons).unwrap();
    let by_uid: HashMap<u32, &mppread::mpp::MppTask> =
        table.tasks.iter().map(|t| (t.uid, t)).collect();
    for &rec in &records {
        let (pred, succ) = (raw::u32_at(rec, 4), raw::u32_at(rec, 8));
        assert!(
            by_uid[&succ]
                .predecessors
                .iter()
                .any(|p| p.pred_uid == pred && u16::from(p.kind) == raw::u16_at(rec, 12)),
            "{at}: link {pred} -> {succ}"
        );
    }
    let links = records.len();
    let decoded: usize = table.tasks.iter().map(|t| t.predecessors.len()).sum();
    let kept: usize = project.tasks.iter().map(|t| t.predecessors.len()).sum();
    assert_eq!((decoded, kept), (links, links), "{at}: links");
    seen.links += links;

    // TBkndRsc and TBkndAssn: the generic reader's live rows, less Project's
    // implicit rows, are the imported resources and assignments.
    let resources = raw::decode_resources(&bytes, table.legacy).unwrap();
    if mpp12 {
        // Project 2007 resources and assignments are not validated.
        assert!(resources.is_none() && project.resources.is_empty(), "{at}");
        assert!(project.assignments.is_empty(), "{at}");
    } else {
        let rsc = raw::RESOURCE_TABLE.read(&bytes).unwrap().unwrap();
        let resources = resources.unwrap();
        let unassigned = rsc.rows.iter().filter(|r| raw::u32_at(r, 4) == 0).count();
        assert_eq!(rsc.rows.len(), resources.len() + unassigned, "{at}");
        assert_eq!(
            resources.iter().map(|r| r.uid).collect::<Vec<_>>(),
            project.resources.iter().map(|r| r.uid).collect::<Vec<_>>(),
            "{at}: resources"
        );
        for r in &resources {
            let name = rsc.field(r.uid as u32, 1).and_then(raw::utf16le_string);
            assert_eq!(name.unwrap_or_default(), r.name, "{at}: resource {}", r.uid);
        }
        let task_uids: HashSet<i32> = project
            .tasks
            .iter()
            .filter(|t| !t.is_null)
            .map(|t| t.uid)
            .chain([0])
            .collect();
        let assignments = raw::decode_assignments(&bytes, false, &task_uids, &resources)
            .unwrap()
            .unwrap();
        let assn = raw::ASSIGNMENT_TABLE.read(&bytes).unwrap().unwrap();
        let placeholders = assn
            .rows
            .iter()
            .filter(|r| raw::i32_at(r, 4) == 0 && raw::i32_at(r, 8) == 0)
            .count();
        assert_eq!(assn.rows.len(), assignments.len() + placeholders, "{at}");
        assert_eq!(
            assignments.iter().map(|a| a.uid).collect::<Vec<_>>(),
            project
                .assignments
                .iter()
                .map(|a| a.uid)
                .collect::<Vec<_>>(),
            "{at}: assignments"
        );
        seen.resources += resources.len();
        seen.assignments += assignments.len();
        // The generic reader keys every VarMeta entry, as var_entries reads it.
        for (layout, read) in [(raw::RESOURCE_TABLE, &rsc), (raw::ASSIGNMENT_TABLE, &assn)] {
            let vm = stream(&cfb, layout.name, "VarMeta").unwrap();
            let v2 = stream(&cfb, layout.name, "Var2Data").unwrap();
            let mut entries: Vec<_> = raw::var_entries(&vm, &v2)
                .unwrap()
                .into_iter()
                .inspect(|e| assert_eq!(e.tag, layout.marker, "{at}: {}", layout.name))
                .map(|e| (e.uid, e.key, e.value))
                .collect();
            entries.sort_by_key(|&(uid, key, _)| (uid, key));
            assert_eq!(read.fields(), entries, "{at}: {}", layout.name);
        }
    }

    // TBkndCal: the decoded calendars are the project's.
    let calendars = raw::decode_calendars(&bytes, table.legacy).unwrap();
    if mpp12 {
        assert!(calendars.is_none(), "{at}");
    } else {
        let decoded = calendars.unwrap();
        assert_eq!(
            decoded.calendars.iter().map(|c| c.uid).collect::<Vec<_>>(),
            project.calendars.iter().map(|c| c.uid).collect::<Vec<_>>(),
            "{at}: calendars"
        );
        assert_eq!(decoded.default_calendar_uid, project.default_calendar_uid);
        let vm = stream(&cfb, "TBkndCal", "VarMeta").unwrap();
        let v2 = stream(&cfb, "TBkndCal", "Var2Data").unwrap();
        for e in raw::var_entries(&vm, &v2).unwrap() {
            assert_eq!(e.tag, 0x0d40, "{at}: calendar tag");
            let named = decoded.calendars.iter().find(|c| c.uid as u32 == e.uid);
            if let (1, Some(calendar)) = (e.key, named) {
                let name = raw::decode_text(&v2, e.offset, e.uid, "calendar name", false);
                assert_eq!(name.unwrap(), calendar.name, "{at}");
            }
        }
        seen.calendars += decoded.calendars.len();

        // The project Props beside the tables.
        let props = cfb.read_path("   114/Props").unwrap();
        let entries = raw::props_entries(&props).unwrap();
        let manual = entries[&raw::NEW_TASKS_ARE_MANUAL];
        assert_eq!(manual != [0, 0], project.new_tasks_are_manual, "{at}");
        let start = entries
            .get(&raw::PROJECT_START)
            .and_then(|v| raw::decode_checked_timestamp(v, 0).unwrap());
        assert_eq!(Ok(start), table.project_start, "{at}");
    }

    // Every TBknd* storage with keyed values parses, one tag per table.
    for path in cfb.paths() {
        let Some(storage) = path.strip_suffix("/VarMeta") else {
            continue;
        };
        if !storage.contains("TBknd") {
            continue;
        }
        let (Some(vm), Some(v2)) = (
            cfb.read_path(&path),
            cfb.read_path(&format!("{storage}/Var2Data")),
        ) else {
            continue;
        };
        let entries = raw::var_entries(&vm, &v2).unwrap_or_else(|e| panic!("{at} {path}: {e}"));
        let tags: HashSet<_> = entries.iter().map(|e| e.tag).collect();
        assert!(tags.len() <= 1, "{at} {path}: tags {tags:x?}");
    }
}

#[test]
fn snapshot_tables_walk_through_raw() {
    let files = snapshots();
    if files.is_empty() {
        eprintln!(
            "skipping: no corpus/mpp/snapshots/*.mpp (see corpus/mpp/README.md to fetch them)"
        );
        return;
    }
    let mut seen = Seen::default();
    for path in &files {
        walk(path, &mut seen);
    }
    eprintln!(
        "raw walk: {} files, {} tasks, {} links, {} resources, {} assignments, {} calendars",
        files.len(),
        seen.tasks,
        seen.links,
        seen.resources,
        seen.assignments,
        seen.calendars
    );
}

const MAGIC: [u8; 4] = [0xba, 0xad, 0xdf, 0xfa];
/// 08:00 on 2025-01-06: 4800 tenths of a minute, day 0x3a86.
const MONDAY_8AM: [u8; 4] = [0xc0, 0x12, 0x86, 0x3a];

/// A Props stream, as Project writes `   114/Props`.
fn props(values: &[(u32, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (key, value) in values {
        body.extend_from_slice(&(value.len() as u32).to_le_bytes());
        body.extend_from_slice(&key.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes());
        body.extend_from_slice(value);
        if value.len() % 2 == 1 {
            body.push(0);
        }
    }
    let len = (body.len() + 12) as u32;
    let mut out = Vec::new();
    for v in [len, len, 0x10000, values.len() as u32] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&body);
    out
}

/// A current Project file of three tasks, the project summary and two
/// tasks linked finish-to-start, built as the task decoder's own fixtures
/// build theirs.
fn tiny_plan() -> Vec<u8> {
    let names = ["Plan", "A", "B"];
    let mut fd = vec![0u8; 48];
    let mut fm = MAGIC.to_vec();
    fm.resize(16, 0);
    fm[8..12].copy_from_slice(&((3 + names.len()) as u32).to_le_bytes());
    for (i, off) in [0u32, 16, 32].into_iter().enumerate() {
        let mut entry = [0u8; raw::TASK_META_STRIDE];
        entry[..2].copy_from_slice(&4u16.to_le_bytes());
        entry[4..8].copy_from_slice(&off.to_le_bytes());
        fm.extend_from_slice(&entry);
        assert_eq!(fm.len(), 16 + (i + 1) * raw::TASK_META_STRIDE);
    }
    let mut vm = MAGIC.to_vec();
    vm.resize(24, 0);
    vm[8..12].copy_from_slice(&(names.len() as u32).to_le_bytes());
    let mut v2 = Vec::new();
    for (uid, name) in names.iter().enumerate() {
        let mut entry = [0u8; raw::TASK_META_STRIDE];
        entry[4..8].copy_from_slice(&(fd.len() as u32).to_le_bytes());
        fm.extend_from_slice(&entry);
        let mut rec = [0u8; raw::TASK_RECORD_LEN];
        rec[..4].copy_from_slice(&(uid as u32).to_le_bytes()); // ID
        rec[4..8].copy_from_slice(&(uid as u32).to_le_bytes()); // UID
        rec[172] = u8::from(uid > 0); // outline level
        rec[162..164].copy_from_slice(&7u16.to_le_bytes()); // LevelingDelayFormat days
        rec[0x68..0x6c].copy_from_slice(&MONDAY_8AM);
        rec[0x6c..0x70].copy_from_slice(&MONDAY_8AM);
        for na in [120, 124, 132, 136, 182] {
            rec[na..na + 4].fill(0xff); // actual dates, resume, stop, deadline
        }
        fd.extend_from_slice(&rec);
        let mut text: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        text.extend_from_slice(&[0, 0]);
        vm.extend_from_slice(&(uid as u32).to_le_bytes());
        vm.extend_from_slice(&(v2.len() as u32).to_le_bytes());
        vm.extend_from_slice(&0x000eu16.to_le_bytes());
        vm.extend_from_slice(&0x0b40u16.to_le_bytes());
        v2.extend_from_slice(&(text.len() as u32).to_le_bytes());
        v2.extend_from_slice(&text);
    }
    vm[20..24].copy_from_slice(&(v2.len() as u32).to_le_bytes());
    let count = (fm.len() - 16) / raw::TASK_META_STRIDE;
    let mut f2m = MAGIC.to_vec();
    f2m.resize(16 + count * raw::FIXED2_META_LEN, 0);
    f2m[8..12].copy_from_slice(&(count as u32).to_le_bytes());
    f2m[12..16].copy_from_slice(&((count * raw::FIXED2_LEN) as u32).to_le_bytes());
    let mut f2d = vec![0u8; count * raw::FIXED2_LEN];
    for i in 0..count {
        let m = 16 + i * raw::FIXED2_META_LEN;
        f2m[m + 4..m + 8].copy_from_slice(&((i * raw::FIXED2_LEN) as u32).to_le_bytes());
        if i >= 3 {
            f2m[m + 8] = 0x40; // active
            let d = i * raw::FIXED2_LEN;
            f2d[d] = i as u8; // a GUID
            f2d[d + 16..d + 24].copy_from_slice(&(i as f64).to_le_bytes()); // sort key
        }
    }
    let mut link = [0u8; raw::LINK_RECORD_LEN];
    link[4..8].copy_from_slice(&1u32.to_le_bytes());
    link[8..12].copy_from_slice(&2u32.to_le_bytes());
    link[12..14].copy_from_slice(&1u16.to_le_bytes()); // finish-to-start
    link[18..20].copy_from_slice(&7u16.to_le_bytes()); // LagFormat days
    write_cfb_tree(&[Node::Storage(
        "   114",
        vec![
            Node::Stream(
                "Props",
                props(&[
                    (raw::NEW_TASKS_ARE_MANUAL, &[0, 0]),
                    (raw::PROJECT_START, &MONDAY_8AM),
                ]),
            ),
            Node::Storage(
                "TBkndTask",
                vec![
                    Node::Stream("FixedMeta", fm),
                    Node::Stream("FixedData", fd),
                    Node::Stream("VarMeta", vm),
                    Node::Stream("Var2Data", v2),
                    Node::Stream("Fixed2Meta", f2m),
                    Node::Stream("Fixed2Data", f2d),
                ],
            ),
            Node::Storage("TBkndCons", vec![Node::Stream("FixedData", link.to_vec())]),
        ],
    )])
}

#[test]
fn tiny_task_table_reads_through_raw() {
    let bytes = tiny_plan();
    let cfb = Cfb::open(&bytes).unwrap();
    let fm = stream(&cfb, "TBkndTask", "FixedMeta").unwrap();
    let fd = stream(&cfb, "TBkndTask", "FixedData").unwrap();
    let raw::TaskIndex::Current(rows) = raw::task_index(&fm, &fd).unwrap() else {
        panic!("not the current layout");
    };
    assert_eq!(
        rows.iter()
            .map(|r| (r.entry, r.id, r.uid, r.offset, r.len, r.is_null))
            .collect::<Vec<_>>(),
        [
            (3, 0, 0, 48, 202, false),
            (4, 1, 1, 250, 202, false),
            (5, 2, 2, 452, 202, false)
        ]
    );
    let f2m = stream(&cfb, "TBkndTask", "Fixed2Meta").unwrap();
    let f2d = stream(&cfb, "TBkndTask", "Fixed2Data").unwrap();
    assert_eq!(raw::task_fixed2(&f2m, &f2d, 6, &rows).unwrap().len(), 3);
    assert!(raw::task_fixed2(&f2m, &f2d, 5, &rows).is_err());

    let vm = stream(&cfb, "TBkndTask", "VarMeta").unwrap();
    let v2 = stream(&cfb, "TBkndTask", "Var2Data").unwrap();
    let names: Vec<_> = raw::var_entries(&vm, &v2)
        .unwrap()
        .into_iter()
        .map(|e| raw::decode_text(&v2, e.offset, e.uid, "name", false).unwrap())
        .collect();
    assert_eq!(names, ["Plan", "A", "B"]);
    assert_eq!(raw::strings(&v2), ["Plan", "A", "B"].map(String::from));

    let props = cfb.read_path("   114/Props").unwrap();
    let entries = raw::props_entries(&props).unwrap();
    assert_eq!(entries[&raw::NEW_TASKS_ARE_MANUAL], [0, 0]);
    assert_eq!(
        raw::decode_timestamp(entries[&raw::PROJECT_START], 0).as_deref(),
        Some("2025-01-06 08:00")
    );

    let table = raw::decode_task_table(&bytes).unwrap();
    assert!(!table.legacy);
    assert_eq!(table.new_tasks_are_manual, Ok(false));
    assert_eq!(table.project_start, Ok(Some("2025-01-06 08:00".into())));
    assert_eq!(
        table
            .tasks
            .iter()
            .map(|t| (t.uid, t.name.as_str(), t.outline_level))
            .collect::<Vec<_>>(),
        [(0, "Plan", Some(0)), (1, "A", Some(1)), (2, "B", Some(1))]
    );
    let link = &table.tasks[2].predecessors;
    assert_eq!(
        (
            link.len(),
            link[0].pred_uid,
            link[0].kind,
            link[0].lag_format
        ),
        (1, 1, 1, 7)
    );

    // The import is built from the same rows.
    let project = project_from_mpp(&bytes).unwrap();
    assert_eq!(project.name, "Plan");
    assert_eq!(
        project.tasks.iter().map(|t| t.uid).collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(project.tasks[1].predecessors[0].uid, 1);
    assert_eq!(raw::decode_resources(&bytes, false), Ok(None));
    assert_eq!(raw::decode_calendars(&bytes, false), Ok(None));
    assert!(!raw::table_present(&bytes, "TBkndAssn").unwrap());
}

#[test]
fn tiny_assignment_table_reads_through_raw() {
    let layout = raw::ASSIGNMENT_TABLE;
    let mut fm = MAGIC.to_vec();
    fm.resize(16 + 2 * layout.meta_stride, 0);
    fm[8..12].copy_from_slice(&2u32.to_le_bytes());
    let mut fd = vec![0u8; 2 * layout.row_len];
    for (i, uid) in [7u32, 8].into_iter().enumerate() {
        let at = 16 + i * layout.meta_stride;
        fm[at..at + 2].copy_from_slice(&(2 * i as u16).to_le_bytes()); // live, superseded
        fm[at + 4..at + 8].copy_from_slice(&((i * layout.row_len) as u32).to_le_bytes());
        fd[i * layout.row_len..i * layout.row_len + 4].copy_from_slice(&uid.to_le_bytes());
    }
    let mut v2 = 8u32.to_le_bytes().to_vec();
    v2.extend_from_slice(&60_000f64.to_le_bytes());
    let mut vm = MAGIC.to_vec();
    vm.resize(24, 0);
    vm[8..12].copy_from_slice(&1u32.to_le_bytes());
    vm[20..24].copy_from_slice(&(v2.len() as u32).to_le_bytes());
    vm.extend_from_slice(&7u32.to_le_bytes());
    vm.extend_from_slice(&0u32.to_le_bytes());
    vm.extend_from_slice(&0x10u16.to_le_bytes()); // Baseline Work
    vm.extend_from_slice(&layout.marker.to_le_bytes());
    let file = |fm: Vec<u8>| {
        write_cfb_tree(&[Node::Storage(
            "   114",
            vec![Node::Storage(
                layout.name,
                vec![
                    Node::Stream("FixedMeta", fm),
                    Node::Stream("FixedData", fd.clone()),
                    Node::Stream("VarMeta", vm.clone()),
                    Node::Stream("Var2Data", v2.clone()),
                ],
            )],
        )])
    };
    let bytes = file(fm.clone());
    assert!(raw::table_present(&bytes, "TBkndAssn").unwrap());
    let table = layout.read(&bytes).unwrap().unwrap();
    assert_eq!(table.rows.len(), 1, "the superseded row is left out");
    assert_eq!(raw::u32_at(&table.rows[0], 0), 7);
    let work = table.field(7, 0x10).unwrap();
    assert_eq!(raw::work_minutes(raw::f64_at(work, 0)), Some(60));
    assert_eq!(table.fields(), [(7, 0x10, work)]);
    let same = raw::read_table(&bytes, layout.name, 34, 110, 0x0f40).unwrap();
    assert_eq!(same.unwrap().rows, table.rows);
    // Another tag is corruption; another entry stride an unvalidated layout.
    assert!(raw::read_table(&bytes, layout.name, 34, 110, 0x0c40).is_err());
    let mut wider = MAGIC.to_vec();
    wider.resize(16 + 2 * 40, 0);
    wider[8..12].copy_from_slice(&2u32.to_le_bytes());
    wider[16 + 40 + 4..16 + 40 + 8].copy_from_slice(&(layout.row_len as u32).to_le_bytes());
    assert!(layout.read(&file(wider)).unwrap().is_none());
}
