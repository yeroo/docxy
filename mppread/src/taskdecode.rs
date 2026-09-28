//! Validated task-table decoding for current Project and MPP9 storage.
use crate::{
    cfb::Cfb,
    fixedmeta,
    mpp::{MppPred, MppProgress, MppTask, MppTaskFields, decode_timestamp},
};
use projcore::mspdi::lag_from_link_lag;
use projcore::{LagFormat, Rate};
use std::collections::{HashMap, HashSet};

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
struct TaskLayout {
    length: usize,
    start: usize,
    finish: usize,
    level: usize,
}
struct CurrentStreams<'a> {
    fm: &'a [u8],
    fd: &'a [u8],
    vm: &'a [u8],
    v2: &'a [u8],
}
const NEWEST: TaskLayout = TaskLayout {
    length: 202,
    start: 0x68,
    finish: 0x6c,
    level: 172,
};
/// Manual scheduling in the newest layout, found by saving one plan with one
/// task auto and then manual (corpus/tools/gen_mpp_manual_cases.py). The mode
/// is a Fixed2Meta flag; the manual start, finish and duration sit in the
/// task's Fixed2Data record, the duration in tenths of a minute followed by
/// its MSPDI DurationFormat code.
const MANUAL_FLAG: (usize, u8) = (8, 0x80);
const MANUAL_START: usize = 50;
const MANUAL_FINISH: usize = 54;
const MANUAL_DURATION: usize = 58;
const MANUAL_DURATION_FORMAT: usize = 62;
/// Recorded progress in the newest layout's 202-byte FixedData record, found
/// by diffing Project's progress cases (corpus/tools/gen_mpp_progress_cases.py)
/// and checked against its MSPDI export of every snapshot, paired plan and
/// progress case. Percents are u16; dates are timestamps (NA = no date);
/// durations are i32 tenths of a minute; work is an f64 in thousandths of a
/// minute; costs are f64s in MSPDI's units. The Start/Finish/Work variances
/// are not stored: Project derives them from the baseline at export.
struct ProgressLayout {
    work: usize,
    actual_work: usize,
    remaining_work: usize,
    cost: usize,
    actual_cost: usize,
    remaining_cost: usize,
    actual_duration: usize,
    duration: usize,
    remaining_duration: usize,
    duration_format: usize,
    calendar_uid: usize,
    percent_complete: usize,
    percent_work_complete: usize,
    actual_start: usize,
    actual_finish: usize,
    resume: usize,
    stop: usize,
}
const NEWEST_PROGRESS: ProgressLayout = ProgressLayout {
    work: 8,
    actual_work: 16,
    remaining_work: 24,
    cost: 32,
    actual_cost: 40,
    remaining_cost: 56,
    actual_duration: 80,
    duration: 84,
    remaining_duration: 88,
    duration_format: 164,
    calendar_uid: 178,
    percent_complete: 92,
    percent_work_complete: 94,
    actual_start: 120,
    actual_finish: 124,
    resume: 132,
    stop: 136,
};
/// PhysicalPercentComplete is a keyed Var2Data block holding a u16. Project
/// writes the block only for a task that has one; without it the value is 0.
const PHYSICAL_PERCENT_KEY: u16 = 0x045f;
/// Current-layout Var2Data RTF task notes, validated against Project's
/// step-07 snapshot and its MSPDI export.
const TASK_NOTES_KEY: u16 = 0x000f;
const LEGACY: TaskLayout = TaskLayout {
    length: 264,
    start: 88,
    finish: 92,
    level: 40,
};
struct LinkLayout {
    lag: usize,
    format: usize,
    /// The lag formats this layout's offsets are validated for.
    format_of: fn(u16) -> Option<LagFormat>,
}
// Lag and format encode as MSPDI's LinkLag and LagFormat, percentages and
// elapsed lags included (#104); the formats projcore can schedule are read.
const NEWEST_LINK: LinkLayout = LinkLayout {
    lag: 14,
    format: 18,
    format_of: |code| LagFormat::from_code(i64::from(code)),
};
// The three MPP9 corpus files have format 7 and zero lag throughout. The
// +16/+14 positions follow their 20-byte records; nonzero lag has no oracle.
const LEGACY_LINK: LinkLayout = LinkLayout {
    lag: 16,
    format: 14,
    format_of: |code| (code == 7).then_some(LagFormat::DAYS),
};

fn links(cfb: &Cfb, prefix: &str, out: &mut [MppTask], layout: LinkLayout) -> Result<(), String> {
    let cons_path = prefix.replace("TBkndTask/", "TBkndCons/FixedData");
    let Some(cons) = cfb.read_path(&cons_path) else {
        return Ok(());
    };
    if !cons.len().is_multiple_of(20) {
        return Err("link record length mismatch".into());
    }
    let positions: HashMap<_, _> = out.iter().enumerate().map(|(i, t)| (t.uid, i)).collect();
    for rec in cons.as_chunks::<20>().0 {
        let pred_uid = u32_at(rec, 4);
        let succ_uid = u32_at(rec, 8);
        if pred_uid == 0 || succ_uid == 0 {
            return Err("link endpoint UID 0 is the project summary".into());
        }
        let kind = u16_at(rec, 12);
        let lag = i32::from_le_bytes(rec[layout.lag..layout.lag + 4].try_into().unwrap());
        let format = u16_at(rec, layout.format);
        if !positions.contains_key(&pred_uid) || !positions.contains_key(&succ_uid) {
            return Err(format!(
                "link refers to unknown UID {pred_uid} or {succ_uid}"
            ));
        }
        let Some(lag_format) = (layout.format_of)(format).filter(|_| kind <= 3) else {
            return Err(format!(
                "unsupported link type {kind} or LagFormat {format}"
            ));
        };
        let succ = positions[&succ_uid];
        out[succ].predecessors.push(MppPred {
            pred_uid,
            kind: kind as u8,
            lag: lag_from_link_lag(i64::from(lag), lag_format),
            lag_format: format,
        });
    }
    Ok(())
}

fn decode_text(
    v2: &[u8],
    off: usize,
    uid: u32,
    what: &str,
    allow_empty: bool,
) -> Result<String, String> {
    let Some(header_end) = off.checked_add(4).filter(|&n| n <= v2.len()) else {
        return Err(format!("{what} Var2Data offset out of range for UID {uid}"));
    };
    let len = u32_at(v2, off) as usize;
    let Some(end) = header_end.checked_add(len).filter(|&n| n <= v2.len()) else {
        return Err(format!("{what} Var2Data block out of range for UID {uid}"));
    };
    let value = &v2[header_end..end];
    if value.len() < 2 || !value.len().is_multiple_of(2) {
        return Err(format!("invalid {what} block for UID {uid}"));
    }
    let units: Vec<u16> = value
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    if *units.last().unwrap() != 0 {
        return Err(format!("unterminated {what} for UID {uid}"));
    }
    let name = String::from_utf16(&units[..units.len() - 1])
        .map_err(|_| format!("invalid UTF-16 {what} for UID {uid}"))?;
    if (!allow_empty && name.is_empty()) || name.chars().any(char::is_control) {
        return Err(format!("invalid {what} for UID {uid}"));
    }
    Ok(name)
}

fn decode_name(v2: &[u8], off: usize, uid: u32) -> Result<String, String> {
    decode_text(v2, off, uid, "task name", false)
}

fn validate_legacy_level(index: usize, uid: u32, level: u32, previous: u32) -> Result<(), String> {
    if (index == 0 && (uid != 0 || level != 0))
        || level > 20
        || (index > 0 && (level == 0 || level > previous + 1))
    {
        return Err(format!(
            "invalid legacy outline level {level} for UID {uid}"
        ));
    }
    Ok(())
}

/// Keyed Var2Data values of the newest layout's tasks.
struct VarFields {
    names: HashMap<u32, String>,
    notes: HashMap<u32, String>,
    physical_percent: HashMap<u32, u8>,
    wbs: HashMap<u32, String>,
}

fn var_fields(vm: &[u8], v2: &[u8], uids: &HashSet<u32>) -> Result<VarFields, String> {
    if vm.len() < 24 || vm[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err("invalid VarMeta header".into());
    }
    let count = u32_at(vm, 8) as usize;
    if 24usize
        .checked_add(count.checked_mul(12).ok_or("VarMeta count overflow")?)
        .is_none_or(|n| n > vm.len())
        || u32_at(vm, 20) as usize != v2.len()
    {
        return Err("VarMeta count or Var2Data length mismatch".into());
    }
    let mut seen = HashSet::new();
    let mut names = HashMap::new();
    let mut notes = HashMap::new();
    let mut physical_percent = HashMap::new();
    let mut wbs = HashMap::new();
    for i in 0..count {
        let e = &vm[24 + i * 12..24 + (i + 1) * 12];
        let uid = u32_at(e, 0);
        let off = u32_at(e, 4) as usize;
        let key = u16_at(e, 8);
        if u16_at(e, 10) != 0x0b40 || !uids.contains(&uid) {
            return Err(format!("invalid VarMeta entry {i}"));
        }
        if !seen.insert((uid, key)) {
            return Err(format!("duplicate VarMeta key ({uid},{key})"));
        }
        let Some(header_end) = off.checked_add(4).filter(|&n| n <= v2.len()) else {
            return Err(format!("Var2Data offset out of range at entry {i}"));
        };
        let len = u32_at(v2, off) as usize;
        let Some(end) = header_end.checked_add(len).filter(|&n| n <= v2.len()) else {
            return Err(format!("Var2Data block out of range at entry {i}"));
        };
        if key == 0x000e {
            names.insert(uid, decode_name(v2, off, uid)?);
        } else if key == TASK_NOTES_KEY {
            notes.insert(uid, crate::rtf::plain_text(&v2[header_end..end], uid)?);
        } else if key == 0x0010 {
            // Explicit WBS override; default WBS is generated from the outline.
            wbs.insert(uid, decode_text(v2, off, uid, "WBS", true)?);
        } else if key == PHYSICAL_PERCENT_KEY {
            let value = &v2[header_end..end];
            let percent = (value.len() == 2)
                .then(|| u16_at(value, 0))
                .filter(|&p| p <= 100)
                .ok_or_else(|| format!("invalid physical percent complete for UID {uid}"))?;
            physical_percent.insert(uid, percent as u8);
        }
    }
    if names.len() != uids.len() {
        return Err("task names do not cover the FixedMeta UIDs".into());
    }
    Ok(VarFields {
        names,
        notes,
        physical_percent,
        wbs,
    })
}

fn guid_text(raw: &[u8]) -> String {
    let a = u32::from_le_bytes(raw[..4].try_into().unwrap());
    let b = u16::from_le_bytes(raw[4..6].try_into().unwrap());
    let c = u16::from_le_bytes(raw[6..8].try_into().unwrap());
    format!(
        "{a:08X}-{b:04X}-{c:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15]
    )
}

/// Offsets and flag bits were diffed from Project's paired task-fields cases.
fn current_fields(
    rec: &[u8],
    meta: &[u8],
    fixed2: fixedmeta::Fixed2<'_>,
    uid: u32,
    wbs: Option<String>,
    notes: Option<String>,
) -> Result<MppTaskFields, String> {
    let task_type = projcore::TaskType::from_code(i64::from(u16_at(rec, 140)))
        .ok_or_else(|| format!("invalid task type for UID {uid}"))?;
    let priority = i32::from(u16_at(rec, 78));
    if priority > 1000 {
        return Err(format!("invalid priority {priority} for UID {uid}"));
    }
    let leveling_delay_format = u16_at(rec, 162);
    if !matches!(leveling_delay_format & !32, 3..=12 | 21) {
        return Err(format!(
            "invalid LevelingDelayFormat {leveling_delay_format} for UID {uid}"
        ));
    }
    Ok(MppTaskFields {
        guid: Some(guid_text(&fixed2.data[..16])),
        create_date: decode_timestamp(rec, 128),
        wbs,
        notes,
        task_type: Some(task_type),
        active: Some(fixed2.meta[8] & 0x40 != 0),
        effort_driven: Some(meta[13] & 0x08 != 0),
        estimated: Some(u16_at(rec, 164) & 32 != 0),
        priority: Some(priority),
        deadline: decode_timestamp(rec, 182),
        level_assignments: Some(meta[16] & 0x04 != 0),
        leveling_can_split: Some(meta[16] & 0x02 != 0),
        leveling_delay: Some(i64::from(u32_at(rec, 70))),
        // f2-values: 3eh/1ew/45m export 6/10/4, and 2ed? exports 40.
        leveling_delay_format: Some(i32::from(leveling_delay_format)),
        ignore_resource_calendar: Some(fixed2.meta[77] & 0x08 != 0),
        // Fixed2 +76 bit 0x40 also appears on a real physical-EV task
        // (progress/p2-work), so it cannot suppress 0x80. Project's UID 0
        // summary alone exports EarnedValueMethod=0 despite both bits set.
        earned_value_method: Some(i32::from(
            uid != 0 && meta[26] & 0x80 == 0 && fixed2.meta[76] & 0x80 != 0,
        )),
        // x-recurring: this bit is set on the recurrence summary and all four
        // occurrences, but on none of 1,723 nonrecurring Project task rows.
        recurring: Some(meta[13] & 0x02 != 0),
        hide_bar: Some(meta[12] & 0x80 != 0),
        rollup: Some(meta[12] & 0x04 != 0),
        is_subproject: Some(meta[15] & 0x02 != 0),
        is_subproject_read_only: Some(meta[15] & 0x80 != 0),
        external_task: Some(meta[15] & 0x40 != 0),
        milestone: Some(meta[10] & 0x02 != 0),
        ..MppTaskFields::default()
    })
}

fn legacy_names(vm: &[u8], v2: &[u8], uids: &HashSet<u32>) -> Result<HashMap<u32, String>, String> {
    if vm.len() < 24 || vm[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err("unrecognized legacy VarMeta layout".into());
    }
    let count = u32_at(vm, 8) as usize;
    let end = 24usize
        .checked_add(
            count
                .checked_mul(8)
                .ok_or("legacy VarMeta count overflow")?,
        )
        .ok_or("legacy VarMeta count overflow")?;
    if end > vm.len() || u32_at(vm, 20) as usize != v2.len() {
        return Err("legacy VarMeta count or Var2Data length mismatch".into());
    }
    let mut seen = HashSet::new();
    let mut out = HashMap::new();
    for e in vm[24..end].as_chunks::<8>().0 {
        let uid = u16_at(e, 0) as u32;
        let key = u16_at(e, 2);
        let off = u32_at(e, 4) as usize;
        if off.checked_add(4).is_none_or(|n| n > v2.len()) {
            return Err("legacy Var2Data offset out of range".into());
        }
        let len = u32_at(v2, off) as usize;
        if off
            .checked_add(4)
            .and_then(|n| n.checked_add(len))
            .is_none_or(|n| n > v2.len())
        {
            return Err("legacy Var2Data block out of range".into());
        }
        if key == 0x0b00 {
            if !seen.insert((uid, key)) {
                return Err(format!("duplicate legacy task name for UID {uid}"));
            }
            if !uids.contains(&uid) {
                return Err(format!("legacy task name references unknown UID {uid}"));
            }
            out.insert(uid, decode_name(v2, off, uid)?);
        }
    }
    if out.len() != uids.len() {
        return Err(format!(
            "legacy task names cover {}/{} UIDs",
            out.len(),
            uids.len()
        ));
    }
    Ok(out)
}

/// A manual task's stored start, finish and duration.
type ManualFields = (Option<String>, Option<String>, Option<i64>);

fn manual_fields(rec: &[u8], uid: u32) -> Result<ManualFields, String> {
    let start = decode_timestamp(rec, MANUAL_START);
    let finish = decode_timestamp(rec, MANUAL_FINISH);
    if start
        .as_ref()
        .zip(finish.as_ref())
        .is_some_and(|(s, f)| s > f)
    {
        return Err(format!("inverted manual dates for UID {uid}"));
    }
    let raw = u32_at(rec, MANUAL_DURATION);
    if raw == u32::MAX {
        return Ok((start, finish, None));
    }
    if raw > i32::MAX as u32 {
        return Err(format!("invalid manual duration for UID {uid}"));
    }
    // DurationFormat without its estimated (`?`) bit.
    let duration = match u16_at(rec, MANUAL_DURATION_FORMAT) & !32 {
        // Manual blank (21) carries a usable duration. An auto blank format
        // falls back to its date span because its stored Duration is unverified.
        format if crate::mpp::working_duration_format(format) || format == 21 => {
            Some(raw as i64 / 10)
        }
        // Elapsed units have no oracle yet: keep the duration unknown.
        4 | 6 | 8 | 10 | 12 => None,
        format => {
            return Err(format!(
                "unrecognized manual DurationFormat {format} for UID {uid}"
            ));
        }
    };
    Ok((start, finish, duration))
}

fn tenths_to_minutes(tenths: i32) -> i64 {
    // Round half away from zero, as MSPDI import rounds a duration's seconds.
    (f64::from(tenths) / 10.0).round() as i64
}

fn duration_at(rec: &[u8], off: usize, what: &str, uid: u32) -> Result<i64, String> {
    let tenths = i32::from_le_bytes(rec[off..off + 4].try_into().unwrap());
    (tenths >= 0)
        .then(|| tenths_to_minutes(tenths))
        .ok_or_else(|| format!("negative {what} for UID {uid}"))
}

fn progress_fields(rec: &[u8], physical_percent: u8, uid: u32) -> Result<MppProgress, String> {
    let at = NEWEST_PROGRESS;
    let percent = |off: usize, what: &str| {
        let value = u16_at(rec, off);
        (value <= 100)
            .then_some(value as u8)
            .ok_or_else(|| format!("invalid {what} {value} for UID {uid}"))
    };
    let f64_at = |off: usize| f64::from_le_bytes(rec[off..off + 8].try_into().unwrap());
    let work = |off: usize, what: &str| {
        let value = f64_at(off);
        (value.is_finite() && value >= 0.0)
            .then(|| (value / 1000.0).round() as i64)
            .ok_or_else(|| format!("invalid {what} for UID {uid}"))
    };
    let cost = |off: usize, what: &str| {
        cost_rate(f64_at(off)).ok_or_else(|| format!("invalid {what} for UID {uid}"))
    };
    let actual_start = decode_timestamp(rec, at.actual_start);
    let actual_finish = decode_timestamp(rec, at.actual_finish);
    if actual_finish.is_some()
        && actual_start
            .as_ref()
            .is_none_or(|s| Some(s) > actual_finish.as_ref())
    {
        return Err(format!(
            "actual finish without an earlier actual start for UID {uid}"
        ));
    }
    Ok(MppProgress {
        percent_complete: percent(at.percent_complete, "percent complete")?,
        percent_work_complete: percent(at.percent_work_complete, "percent work complete")?,
        physical_percent_complete: physical_percent,
        actual_start,
        actual_finish,
        stop: decode_timestamp(rec, at.stop),
        resume: decode_timestamp(rec, at.resume),
        actual_duration_min: duration_at(rec, at.actual_duration, "actual duration", uid)?,
        remaining_duration_min: duration_at(rec, at.remaining_duration, "remaining duration", uid)?,
        work_min: work(at.work, "work")?,
        actual_work_min: work(at.actual_work, "actual work")?,
        remaining_work_min: work(at.remaining_work, "remaining work")?,
        cost: cost(at.cost, "cost")?,
        actual_cost: cost(at.actual_cost, "actual cost")?,
        remaining_cost: cost(at.remaining_cost, "remaining cost")?,
    })
}

/// A stored cost as Project's MSPDI export writes it: rounded to two
/// decimals, without trailing zeros (`9319.5`, `3406`).
fn cost_rate(value: f64) -> Option<Rate> {
    if !value.is_finite() {
        return None;
    }
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    Rate::parse(if text == "-0" { "0" } else { text })
}

/// A validated task table and the project's new-task mode. The mode is kept
/// as its own result so a bad project option refuses an import but not
/// [`decode`].
pub(crate) struct Table {
    pub tasks: Vec<MppTask>,
    pub new_tasks_are_manual: Result<bool, String>,
    pub legacy: bool,
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<MppTask>, String> {
    decode_table(bytes).map(|table| table.tasks)
}

/// The project's new-task mode; see [`crate::mpp::decode_new_tasks_are_manual`].
pub(crate) fn new_tasks_are_manual(bytes: &[u8]) -> Result<bool, String> {
    decode_table(bytes)?.new_tasks_are_manual
}

/// Decode the task table. MPP9 predates manual tasks and a file without a
/// task table has nothing to default, so both have an auto default; the
/// newest layout reads it from the project Props beside the table.
pub(crate) fn decode_table(bytes: &[u8]) -> Result<Table, String> {
    let cfb = Cfb::open(bytes)?;
    let paths = cfb.paths();
    let task_paths: Vec<_> = paths.iter().filter(|p| p.contains("TBkndTask/")).collect();
    if task_paths.is_empty() {
        return Ok(Table {
            tasks: Vec::new(),
            new_tasks_are_manual: Ok(false),
            legacy: false,
        });
    }
    let Some(meta_path) = paths.iter().find(|p| p.ends_with("TBkndTask/FixedMeta")) else {
        return Err("partial task stream set: missing FixedMeta".into());
    };
    let prefix = meta_path.trim_end_matches("FixedMeta");
    let read = |name: &str| {
        cfb.read_path(&format!("{prefix}{name}"))
            .ok_or_else(|| format!("partial task stream set: missing {name}"))
    };
    let fm = read("FixedMeta")?;
    let fd = read("FixedData")?;
    let vm = read("VarMeta")?;
    let v2 = read("Var2Data")?;
    match fixedmeta::index(&fm, &fd)? {
        fixedmeta::TaskIndex::Current(indexed) => {
            let fixed_count = u32_at(&fm, 8) as usize;
            let (f2m, f2d) = (read("Fixed2Meta")?, read("Fixed2Data")?);
            let fixed2 = fixedmeta::current_fixed2(&f2m, &f2d, fixed_count, &indexed)?;
            let props = prefix.trim_end_matches("TBkndTask/").to_string() + "Props";
            let props_data = cfb.read_path(&props);
            // A custom Project WBS mask changes generated codes. Preserve explicit
            // per-task codes, but leave other codes unknown until that mask is decoded.
            let default_wbs_mask = props_data
                .as_ref()
                // Malformed Props makes WBS unknown here; the project import
                // reports the Props error via NewTasksAreManual below.
                .and_then(|p| crate::props::has_default_wbs_mask(p).ok())
                .unwrap_or(false);
            let mut tasks = decode_current(
                &cfb,
                prefix,
                CurrentStreams {
                    fm: &fm,
                    fd: &fd,
                    vm: &vm,
                    v2: &v2,
                },
                indexed,
                &fixed2,
                default_wbs_mask,
            )?;
            if let Ok(overallocated) = crate::overalloc::decode(&cfb, &tasks) {
                for task in &mut tasks {
                    if let Some(fields) = &mut task.fields {
                        fields.over_allocated = overallocated.get(&task.uid).copied();
                    }
                }
            }
            let new_tasks_are_manual = props_data
                .ok_or_else(|| "missing project Props stream".to_string())
                .and_then(|props| crate::props::new_tasks_are_manual(&props));
            Ok(Table {
                tasks,
                new_tasks_are_manual,
                legacy: false,
            })
        }
        fixedmeta::TaskIndex::Legacy(indexed) => {
            let uids: HashSet<_> = indexed.iter().map(|r| r.uid).collect();
            Ok(Table {
                tasks: decode_legacy(&cfb, prefix, &fd, &vm, &v2, indexed, &uids)?,
                new_tasks_are_manual: Ok(false),
                legacy: true,
            })
        }
    }
}

fn decode_current(
    cfb: &Cfb,
    prefix: &str,
    streams: CurrentStreams<'_>,
    indexed: Vec<fixedmeta::CurrentRecord>,
    fixed2: &[fixedmeta::Fixed2],
    default_wbs_mask: bool,
) -> Result<Vec<MppTask>, String> {
    let CurrentStreams { fm, fd, vm, v2 } = streams;
    let uids: HashSet<_> = indexed
        .iter()
        .filter(|r| !r.is_null)
        .map(|r| r.uid)
        .collect();
    let var = var_fields(vm, v2, &uids)?;
    let mut out = Vec::new();
    let mut wbs_parts = [0u32; 21];
    for (row, fixed2) in indexed.into_iter().zip(fixed2) {
        if row.is_null {
            out.push(MppTask {
                id: row.id,
                uid: row.uid,
                is_null: true,
                ..MppTask::default()
            });
            continue;
        }
        if row.len != NEWEST.length {
            return Err(format!("unrecognized task record length {}", row.len));
        }
        let rec = &fd[row.offset..row.offset + row.len];
        let start = decode_timestamp(rec, NEWEST.start);
        let finish = decode_timestamp(rec, NEWEST.finish);
        let level = rec[NEWEST.level] as u32;
        if level > 20 {
            return Err(format!("invalid outline level {level} for UID {}", row.uid));
        }
        if start
            .as_ref()
            .zip(finish.as_ref())
            .is_none_or(|(s, f)| s > f)
        {
            return Err(format!(
                "missing or inverted task dates for UID {}",
                row.uid
            ));
        }
        let manual = fixed2.meta[MANUAL_FLAG.0] & MANUAL_FLAG.1 != 0;
        let (manual_start, manual_finish, manual_duration_min) = if manual {
            manual_fields(fixed2.data, row.uid)?
        } else {
            (None, None, None)
        };
        let physical_percent = var.physical_percent.get(&row.uid).copied().unwrap_or(0);
        let duration_min = duration_at(rec, NEWEST_PROGRESS.duration, "duration", row.uid)?;
        let progress = progress_fields(rec, physical_percent, row.uid)?;
        if level > 0 {
            wbs_parts[level as usize] += 1;
            wbs_parts[level as usize + 1..].fill(0);
        }
        let default_wbs = if level == 0 {
            "0".to_string()
        } else {
            (1..=level as usize)
                .map(|i| wbs_parts[i].to_string())
                .collect::<Vec<_>>()
                .join(".")
        };
        let meta = &fm[16 + row.entry * 47..16 + (row.entry + 1) * 47];
        out.push(MppTask {
            id: row.id,
            uid: row.uid,
            name: var.names[&row.uid].clone(),
            start,
            finish,
            outline_level: Some(level),
            predecessors: Vec::new(),
            manual,
            manual_start,
            manual_finish,
            manual_duration_min,
            duration_min: Some(duration_min),
            duration_format: Some(u16_at(rec, NEWEST_PROGRESS.duration_format)),
            calendar_uid: Some(i32::from_le_bytes(
                rec[NEWEST_PROGRESS.calendar_uid..NEWEST_PROGRESS.calendar_uid + 4]
                    .try_into()
                    .unwrap(),
            )),
            progress: Some(progress),
            fields: Some(current_fields(
                rec,
                meta,
                *fixed2,
                row.uid,
                var.wbs
                    .get(&row.uid)
                    .cloned()
                    .or_else(|| default_wbs_mask.then_some(default_wbs)),
                var.notes.get(&row.uid).cloned(),
            )?),
            ..MppTask::default()
        });
    }
    links(cfb, prefix, &mut out, NEWEST_LINK)?;
    Ok(out)
}

fn decode_legacy(
    cfb: &Cfb,
    prefix: &str,
    fd: &[u8],
    vm: &[u8],
    v2: &[u8],
    indexed: Vec<fixedmeta::LegacyRecord>,
    uids: &HashSet<u32>,
) -> Result<Vec<MppTask>, String> {
    let named = legacy_names(vm, v2, uids)?;
    let mut out = Vec::new();
    let mut previous_level = 0u32;
    for (i, row) in indexed.iter().enumerate() {
        if row.len != LEGACY.length {
            return Err("legacy task record length mismatch".into());
        }
        let uid = row.uid;
        let rec = &fd[row.offset..row.offset + row.len];
        let level = rec[LEGACY.level] as u32;
        validate_legacy_level(i, uid, level, previous_level)?;
        previous_level = level;
        let start = decode_timestamp(rec, LEGACY.start);
        let finish = decode_timestamp(rec, LEGACY.finish);
        if start
            .as_ref()
            .zip(finish.as_ref())
            .is_none_or(|(s, f)| s > f)
        {
            return Err(format!(
                "missing or inverted legacy task dates for UID {uid}"
            ));
        }
        out.push(MppTask {
            id: row.id,
            uid,
            name: named[&uid].clone(),
            start,
            finish,
            outline_level: Some(level),
            predecessors: Vec::new(),
            // Project 2003 has no manual scheduling.
            ..MppTask::default()
        });
    }
    links(cfb, prefix, &mut out, LEGACY_LINK)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cfb::{Node, write_cfb_tree};

    #[test]
    fn legacy_accepts_short_non_latin_names_and_flat_children() {
        let mut block = 6u32.to_le_bytes().to_vec();
        for unit in "中文".encode_utf16() {
            block.extend_from_slice(&unit.to_le_bytes());
        }
        block.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(decode_name(&block, 0, 1).unwrap(), "中文");
        let mut previous = 0;
        for (i, level) in [0, 1, 2, 2].into_iter().enumerate() {
            validate_legacy_level(i, i as u32, level, previous).unwrap();
            previous = level;
        }
        assert!(validate_legacy_level(2, 2, 3, 1).is_err());
    }

    struct Streams {
        fm: Vec<u8>,
        fd: Vec<u8>,
        vm: Vec<u8>,
        v2: Vec<u8>,
        cons: Vec<u8>,
        f2m: Vec<u8>,
        f2d: Vec<u8>,
        props: Vec<u8>,
    }
    fn fixture() -> Streams {
        let mut fd = vec![0u8; 452];
        fd[0..4].copy_from_slice(&0u32.to_le_bytes());
        fd[16..20].copy_from_slice(&1u32.to_le_bytes());
        fd[32..36].copy_from_slice(&2u32.to_le_bytes());
        for (i, uid) in [0u32, 1].into_iter().enumerate() {
            let o = 48 + i * 202;
            fd[o..o + 4].copy_from_slice(&uid.to_le_bytes());
            fd[o + 4..o + 8].copy_from_slice(&uid.to_le_bytes());
            fd[o + 172] = i as u8;
            fd[o + 162..o + 164].copy_from_slice(&8u16.to_le_bytes());
            for d in [0x68, 0x6c] {
                fd[o + d..o + d + 4].copy_from_slice(&[0xc0, 0x12, 0x86, 0x3a]);
            }
        }
        let mut fm = vec![0u8; 16 + 5 * 47];
        fm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        fm[8..12].copy_from_slice(&5u32.to_le_bytes());
        for (i, off) in [0u32, 16, 32, 48, 250].into_iter().enumerate() {
            let p = 16 + i * 47;
            fm[p + 4..p + 8].copy_from_slice(&off.to_le_bytes());
            if i < 3 {
                fm[p..p + 2].copy_from_slice(&4u16.to_le_bytes());
            }
        }
        let mut vm = vec![0u8; 24];
        vm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        vm[8..12].copy_from_slice(&2u32.to_le_bytes());
        let mut v2 = Vec::new();
        for (uid, name) in [(0u32, 'A'), (1u32, 'B')] {
            let off = v2.len() as u32;
            v2.extend_from_slice(&4u32.to_le_bytes());
            v2.extend_from_slice(&(name as u16).to_le_bytes());
            v2.extend_from_slice(&0u16.to_le_bytes());
            vm.extend_from_slice(&uid.to_le_bytes());
            vm.extend_from_slice(&off.to_le_bytes());
            vm.extend_from_slice(&0x000eu16.to_le_bytes());
            vm.extend_from_slice(&0x0b40u16.to_le_bytes());
        }
        vm[20..24].copy_from_slice(&(v2.len() as u32).to_le_bytes());
        let (f2m, f2d) = fixed2_for(&fm);
        Streams {
            fm,
            fd,
            vm,
            v2,
            cons: Vec::new(),
            f2m,
            f2d,
            props: props(&[0, 0]),
        }
    }
    /// Fixed2 streams that match `fm`: a GUID and a rising sort key for each
    /// task entry, nothing for the schema stubs and blank rows.
    fn fixed2_for(fm: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let count = (fm.len() - 16) / 47;
        let mut meta = vec![0u8; 16 + count * 96];
        meta[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        meta[8..12].copy_from_slice(&(count as u32).to_le_bytes());
        meta[12..16].copy_from_slice(&((count * 64) as u32).to_le_bytes());
        let mut data = vec![0u8; count * 64];
        for i in 0..count {
            let m = 16 + i * 96;
            meta[m + 4..m + 8].copy_from_slice(&((i * 64) as u32).to_le_bytes());
            if i >= 3 && fm[16 + i * 47..16 + i * 47 + 2] == [0, 0] {
                data[i * 64] = i as u8;
                data[i * 64 + 16..i * 64 + 24].copy_from_slice(&(i as f64).to_le_bytes());
            }
        }
        (meta, data)
    }
    fn props(new_tasks_are_manual: &[u8]) -> Vec<u8> {
        crate::props::stream(&[(crate::props::NEW_TASKS_ARE_MANUAL, new_tasks_are_manual)])
    }

    fn outline_fixture() -> Streams {
        let mut s = fixture();
        s.fd.truncate(48);
        s.fm.truncate(16 + 3 * 47);
        s.vm.truncate(24);
        s.v2.clear();
        let rows: [(u32, u32, u8, Option<&str>); 6] = [
            (0, 0, 0, Some("Project")),
            (1, 1, 1, Some("One")),
            (2, 2, 2, Some("Child A")),
            (5, 3, 0, None),
            (3, 4, 2, Some("Child B")),
            (4, 5, 1, Some("Two")),
        ];
        for (entry, (uid, id, level, name)) in rows.into_iter().enumerate() {
            let offset = s.fd.len() as u32;
            let mut meta = [0u8; 47];
            meta[4..8].copy_from_slice(&offset.to_le_bytes());
            if let Some(name) = name {
                let mut rec = [0u8; 202];
                rec[..4].copy_from_slice(&id.to_le_bytes());
                rec[4..8].copy_from_slice(&uid.to_le_bytes());
                rec[172] = level;
                rec[162..164].copy_from_slice(&8u16.to_le_bytes());
                for d in [0x68, 0x6c] {
                    rec[d..d + 4].copy_from_slice(&[0xc0, 0x12, 0x86, 0x3a]);
                }
                s.fd.extend_from_slice(&rec);
                let off = s.v2.len() as u32;
                let mut value: Vec<_> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
                value.extend_from_slice(&0u16.to_le_bytes());
                s.v2.extend_from_slice(&(value.len() as u32).to_le_bytes());
                s.v2.extend_from_slice(&value);
                s.vm.extend_from_slice(&uid.to_le_bytes());
                s.vm.extend_from_slice(&off.to_le_bytes());
                s.vm.extend_from_slice(&0x000eu16.to_le_bytes());
                s.vm.extend_from_slice(&0x0b40u16.to_le_bytes());
            } else {
                meta[..2].copy_from_slice(&4u16.to_le_bytes());
                s.fd.extend_from_slice(&uid.to_le_bytes());
                s.fd.extend_from_slice(&id.to_le_bytes());
                s.fd.extend_from_slice(&[0; 8]);
            }
            assert_eq!(s.fm.len(), 16 + (3 + entry) * 47);
            s.fm.extend_from_slice(&meta);
        }
        s.fm[8..12].copy_from_slice(&9u32.to_le_bytes());
        s.vm[8..12].copy_from_slice(&5u32.to_le_bytes());
        s.vm[20..24].copy_from_slice(&(s.v2.len() as u32).to_le_bytes());
        (s.f2m, s.f2d) = fixed2_for(&s.fm);
        s
    }
    fn file(s: &Streams, include_fd: bool) -> Vec<u8> {
        let mut task = vec![
            Node::Stream("FixedMeta", s.fm.clone()),
            Node::Stream("VarMeta", s.vm.clone()),
            Node::Stream("Var2Data", s.v2.clone()),
            Node::Stream("Fixed2Meta", s.f2m.clone()),
            Node::Stream("Fixed2Data", s.f2d.clone()),
        ];
        if include_fd {
            task.push(Node::Stream("FixedData", s.fd.clone()));
        }
        write_cfb_tree(&[Node::Storage(
            "   114",
            vec![
                Node::Stream("Props", s.props.clone()),
                Node::Storage("TBkndTask", task),
                Node::Storage("TBkndCons", vec![Node::Stream("FixedData", s.cons.clone())]),
            ],
        )])
    }
    fn file_with_calendar(s: &Streams, closed: bool, work_week_uids: &[i32]) -> Vec<u8> {
        let (cal_fm, cal_fd, mut cal_vm, mut cal_v2) = crate::caldecode::tests::fixture();
        let original_v2 = cal_v2;
        cal_v2 = Vec::new();
        for index in 0..4 {
            let entry = 24 + index * 12;
            let uid = i32::from_le_bytes(cal_vm[entry..entry + 4].try_into().unwrap());
            let key = u16::from_le_bytes(cal_vm[entry + 8..entry + 10].try_into().unwrap());
            let old_off =
                u32::from_le_bytes(cal_vm[entry + 4..entry + 8].try_into().unwrap()) as usize;
            let len =
                u32::from_le_bytes(original_v2[old_off..old_off + 4].try_into().unwrap()) as usize;
            let mut value = original_v2[old_off + 4..old_off + 4 + len].to_vec();
            if key == 8 && work_week_uids.contains(&uid) {
                value[424..428].copy_from_slice(&1u32.to_le_bytes());
                value.extend_from_slice(&crate::caldecode::tests::work_week_record());
            }
            cal_vm[entry + 4..entry + 8].copy_from_slice(&(cal_v2.len() as u32).to_le_bytes());
            cal_v2.extend_from_slice(&(value.len() as u32).to_le_bytes());
            cal_v2.extend_from_slice(&value);
        }
        cal_vm[20..24].copy_from_slice(&(cal_v2.len() as u32).to_le_bytes());
        if closed {
            // The fourth VarMeta entry is UID 5's weekday block. Override
            // every inherited day with a day having zero working periods.
            let off = u32::from_le_bytes(cal_vm[64..68].try_into().unwrap()) as usize + 4;
            for day in 0..7 {
                cal_v2[off + day * 60..off + (day + 1) * 60].fill(0);
            }
        }
        let default: Vec<_> = "Standard"
            .encode_utf16()
            .chain([0, 0])
            .flat_map(u16::to_le_bytes)
            .collect();
        let props = crate::props::stream(&[
            (crate::props::NEW_TASKS_ARE_MANUAL, &[0, 0]),
            (0x0240_000e, default.as_slice()),
        ]);
        write_cfb_tree(&[Node::Storage(
            "   114",
            vec![
                Node::Stream("Props", props),
                Node::Storage(
                    "TBkndTask",
                    vec![
                        Node::Stream("FixedMeta", s.fm.clone()),
                        Node::Stream("FixedData", s.fd.clone()),
                        Node::Stream("VarMeta", s.vm.clone()),
                        Node::Stream("Var2Data", s.v2.clone()),
                        Node::Stream("Fixed2Meta", s.f2m.clone()),
                        Node::Stream("Fixed2Data", s.f2d.clone()),
                    ],
                ),
                Node::Storage("TBkndCons", vec![Node::Stream("FixedData", s.cons.clone())]),
                Node::Storage(
                    "TBkndCal",
                    vec![
                        Node::Stream("FixedMeta", cal_fm),
                        Node::Stream("FixedData", cal_fd),
                        Node::Stream("VarMeta", cal_vm),
                        Node::Stream("Var2Data", cal_v2),
                    ],
                ),
            ],
        )])
    }
    fn reject(s: &Streams) {
        assert!(decode(&file(s, true)).is_err());
    }

    #[test]
    fn missing_optional_assignment_table_keeps_task_import() {
        let bytes = file(&fixture(), true);
        let project = crate::project::project_from_mpp(&bytes).unwrap();
        assert!(project.tasks.iter().all(|t| t.over_allocated.is_none()));
    }

    #[test]
    fn keyed_single_letter_names_and_declared_count() {
        let mut s = fixture();
        let tasks = decode(&file(&s, true)).unwrap();
        assert_eq!(
            tasks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["A", "B"]
        );
        // Stale bytes after the declared table have no standing as entries.
        s.vm.extend_from_within(24..36);
        assert!(decode(&file(&s, true)).is_ok());
        s.vm[8..12].copy_from_slice(&4u32.to_le_bytes());
        reject(&s);
    }
    #[test]
    fn derives_wbs_across_depth_pop_and_blank_only_for_default_mask() {
        let mut s = outline_fixture();
        let default = decode(&file(&s, true)).unwrap();
        assert_eq!(
            default
                .iter()
                .map(|t| t.fields.as_ref().and_then(|f| f.wbs.as_deref()))
                .collect::<Vec<_>>(),
            [
                Some("0"),
                Some("1"),
                Some("1.1"),
                None,
                Some("1.2"),
                Some("2")
            ]
        );
        s.props = crate::props::stream(&[
            (crate::props::NEW_TASKS_ARE_MANUAL, &[0, 0]),
            (0x0240_138b, &[1, 0, 0, 0]),
        ]);
        let custom = decode(&file(&s, true)).unwrap();
        assert!(
            custom
                .iter()
                .all(|t| t.fields.as_ref().is_none_or(|f| f.wbs.is_none()))
        );
    }
    #[test]
    fn null_rows_are_identified_by_fixedmeta_kind_and_count_for_id_gaps() {
        let mut s = fixture();
        s.fd[250..254].copy_from_slice(&2u32.to_le_bytes()); // real B is row 2
        let mut null = [0u8; 16];
        null[0..4].copy_from_slice(&4u32.to_le_bytes()); // null UID
        null[4..8].copy_from_slice(&1u32.to_le_bytes()); // null row ID
        s.fd.extend_from_slice(&null);
        s.fm[8..12].copy_from_slice(&6u32.to_le_bytes());
        s.fm.extend_from_slice(&[0u8; 47]);
        let p = 16 + 5 * 47;
        s.fm[p..p + 2].copy_from_slice(&4u16.to_le_bytes());
        s.fm[p + 4..p + 8].copy_from_slice(&452u32.to_le_bytes());
        (s.f2m, s.f2d) = fixed2_for(&s.fm);
        let decoded = decode(&file(&s, true)).unwrap();
        assert_eq!(
            decoded.iter().map(|t| (t.id, t.uid)).collect::<Vec<_>>(),
            [(0, 0), (1, 4), (2, 1)]
        );
        let imported = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        assert_eq!(
            imported
                .tasks
                .iter()
                .map(|t| (t.id, t.uid, t.is_null))
                .collect::<Vec<_>>(),
            [(1, 4, true), (2, 1, false)]
        );
        let xml = projcore::mspdi::write_mspdi(&imported);
        let reread = projcore::mspdi::read_mspdi(&xml).unwrap();
        assert_eq!(
            reread
                .tasks
                .iter()
                .map(|t| (t.id, t.uid, t.is_null))
                .collect::<Vec<_>>(),
            [(1, 4, true), (2, 1, false)]
        );
        s.fm[p..p + 2].copy_from_slice(&0u16.to_le_bytes());
        reject(&s); // a short record without the null marker is malformed
        s.fm[p..p + 2].copy_from_slice(&4u16.to_le_bytes());
        s.fd[452..456].copy_from_slice(&1u32.to_le_bytes());
        reject(&s); // duplicate null UID is malformed
        s.fd[452..456].copy_from_slice(&4u32.to_le_bytes());
        s.fd[456..460].copy_from_slice(&4u32.to_le_bytes());
        reject(&s); // task IDs must be contiguous even across null rows
    }
    #[test]
    fn conversion_preserves_uid_and_uses_summary_name() {
        let mut s = fixture();
        s.fd[254..258].copy_from_slice(&7u32.to_le_bytes());
        s.vm[36..40].copy_from_slice(&7u32.to_le_bytes());
        let project = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        assert_eq!(project.name, "A");
        assert_eq!(project.tasks.len(), 1);
        assert_eq!(project.tasks[0].uid, 7);
        assert_eq!(project.tasks[0].name, "B");
        let err = crate::project::project_from_mpp(&file(&s, false)).unwrap_err();
        assert!(err.starts_with("cannot read the task table of this .mpp ("));
    }
    #[test]
    fn task_field_bits_values_and_round_trip() {
        let mut s = fixture();
        let m = 16 + 4 * 47;
        s.fm[m + 10] |= 0x02; // milestone, f1-flags
        s.fm[m + 12] |= 0x84; // hide bar, rollup, f1-flags
        s.fm[m + 13] |= 0x08; // effort driven, f1-flags
        s.fm[m + 15] |= 0x82; // subproject, read-only, f6-subprojects
        s.fm[m + 16] |= 0x07; // leveling options, f1-flags
        let d = 250;
        s.fd[d + 70..d + 74].copy_from_slice(&28800u32.to_le_bytes());
        s.fd[d + 78..d + 80].copy_from_slice(&317u16.to_le_bytes());
        s.fd[d + 128..d + 132].copy_from_slice(&[0xc0, 0x12, 0x86, 0x3a]);
        s.fd[d + 140..d + 142].copy_from_slice(&2u16.to_le_bytes());
        s.fd[d + 164..d + 166].copy_from_slice(&39u16.to_le_bytes());
        s.fd[d + 182..d + 186].copy_from_slice(&[0xc0, 0x12, 0x86, 0x3a]);
        let f = 16 + 4 * 96;
        s.f2m[f + 8] |= 0x40; // active, f1-flags
        s.f2m[f + 76] |= 0x80; // physical EV, f2-values
        s.f2m[f + 77] |= 0x08; // ignore resource calendar, f1-flags
        let off = s.v2.len() as u32;
        let value: Vec<u8> = "CUSTOM\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        s.v2.extend_from_slice(&(value.len() as u32).to_le_bytes());
        s.v2.extend_from_slice(&value);
        s.vm.extend_from_slice(&1u32.to_le_bytes());
        s.vm.extend_from_slice(&off.to_le_bytes());
        s.vm.extend_from_slice(&0x0010u16.to_le_bytes());
        s.vm.extend_from_slice(&0x0b40u16.to_le_bytes());
        s.vm[8..12].copy_from_slice(&3u32.to_le_bytes());
        s.vm[20..24].copy_from_slice(&(s.v2.len() as u32).to_le_bytes());
        let bytes = file(&s, true);
        let task = decode(&bytes).unwrap().remove(1);
        let fields = task.fields.unwrap();
        assert_eq!(fields.wbs.as_deref(), Some("CUSTOM"));
        assert_eq!(fields.task_type, Some(projcore::TaskType::FixedWork));
        assert_eq!(fields.priority, Some(317));
        assert_eq!(fields.leveling_delay, Some(28800));
        assert_eq!(fields.deadline, task.start);
        assert_eq!(
            (fields.active, fields.effort_driven, fields.estimated),
            (Some(true), Some(true), Some(true))
        );
        assert_eq!(
            (fields.hide_bar, fields.rollup, fields.milestone),
            (Some(true), Some(true), Some(true))
        );
        assert_eq!(
            (fields.is_subproject, fields.is_subproject_read_only),
            (Some(true), Some(true))
        );
        assert_eq!(
            (fields.ignore_resource_calendar, fields.earned_value_method),
            (Some(true), Some(1))
        );
        let imported = crate::project::project_from_mpp(&bytes).unwrap();
        let xml = projcore::mspdi::write_mspdi(&imported);
        let reread = projcore::mspdi::read_mspdi(&xml).unwrap();
        let (a, b) = (&imported.tasks[0], &reread.tasks[0]);
        assert_eq!(
            (
                a.guid.clone(),
                a.create_date,
                a.wbs.clone(),
                a.task_type,
                a.active,
                a.effort_driven,
                a.estimated,
                a.priority,
                a.deadline
            ),
            (
                b.guid.clone(),
                b.create_date,
                b.wbs.clone(),
                b.task_type,
                b.active,
                b.effort_driven,
                b.estimated,
                b.priority,
                b.deadline
            )
        );
        assert_eq!(
            (
                a.level_assignments,
                a.leveling_can_split,
                a.leveling_delay,
                a.leveling_delay_format,
                a.ignore_resource_calendar,
                a.earned_value_method
            ),
            (
                b.level_assignments,
                b.leveling_can_split,
                b.leveling_delay,
                b.leveling_delay_format,
                b.ignore_resource_calendar,
                b.earned_value_method
            )
        );
        assert_eq!(
            (
                a.hide_bar,
                a.rollup,
                a.external_task,
                a.is_subproject,
                a.is_subproject_read_only,
                a.over_allocated,
                a.milestone
            ),
            (
                b.hide_bar,
                b.rollup,
                b.external_task,
                b.is_subproject,
                b.is_subproject_read_only,
                b.over_allocated,
                b.milestone
            )
        );
    }

    #[test]
    fn task_notes_decode_and_survive_mspdi_round_trip() {
        let mut s = fixture();
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1]
                .fields
                .as_ref()
                .unwrap()
                .notes,
            None
        );
        // This is the exact 314-byte RichEdit value in snapshot step 07, UID 8.
        let note = b"{\\rtf1\\ansi\\ansicpg1252\\deff0\\nouicompat\\deflang1033{\\fonttbl{\\f0\\fnil\\fcharset0 Segoe UI;}{\\f1\\fnil Segoe UI;}{\\f2\\fnil\\fcharset1 Segoe UI Symbol;}}\r\n{\\*\\generator Riched20 16.0.20026}\\viewkind4\\uc1 \r\n\\pard\\f0\\fs20 First line.\\par\r\nSecond line \\f1\\emdash  unicode \\f2\\u10003?\\f0  and \\u171?quotes\\u187?.\\par\r\n}\r\n\0";
        assert_eq!(note.len(), 314);
        let off = s.v2.len() as u32;
        add_var(&mut s, 1, TASK_NOTES_KEY, note);
        let bytes = file(&s, true);
        let expected = "First line.\r\nSecond line — unicode ✓ and «quotes».";
        assert_eq!(
            decode(&bytes).unwrap()[1]
                .fields
                .as_ref()
                .unwrap()
                .notes
                .as_deref(),
            Some(expected)
        );
        let imported = crate::project::project_from_mpp(&bytes).unwrap();
        assert_eq!(imported.tasks[0].notes.as_deref(), Some(expected));
        let reread = projcore::mspdi::read_mspdi(&projcore::mspdi::write_mspdi(&imported)).unwrap();
        assert_eq!(reread.tasks[0].notes.as_deref(), Some(expected));

        s.v2[off as usize + 4] = b'!';
        assert!(
            decode(&file(&s, true))
                .unwrap_err()
                .contains("invalid notes for UID 1")
        );
    }

    #[test]
    fn task_field_flags_use_independent_bits() {
        type Read = fn(&MppTaskFields) -> Option<bool>;
        let cases: &[(usize, usize, u8, Read)] = &[
            (0, 10, 0x02, |f| f.milestone),
            (0, 12, 0x80, |f| f.hide_bar),
            (0, 12, 0x04, |f| f.rollup),
            (0, 13, 0x08, |f| f.effort_driven),
            (0, 15, 0x02, |f| f.is_subproject),
            (0, 15, 0x80, |f| f.is_subproject_read_only),
            (0, 15, 0x40, |f| f.external_task),
            (0, 16, 0x04, |f| f.level_assignments),
            (0, 16, 0x02, |f| f.leveling_can_split),
            (1, 8, 0x40, |f| f.active),
            (1, 77, 0x08, |f| f.ignore_resource_calendar),
        ];
        for &(block, offset, bit, read) in cases {
            let mut s = fixture();
            let before = decode(&file(&s, true)).unwrap().remove(1).fields.unwrap();
            let pos = if block == 0 {
                16 + 4 * 47 + offset
            } else {
                16 + 4 * 96 + offset
            };
            let meta = if block == 0 { &mut s.fm } else { &mut s.f2m };
            meta[pos] ^= bit;
            let after = decode(&file(&s, true)).unwrap().remove(1).fields.unwrap();
            assert_eq!(
                read(&before),
                Some(false),
                "block {block} offset {offset} bit {bit:#x}"
            );
            assert_eq!(
                read(&after),
                Some(true),
                "block {block} offset {offset} bit {bit:#x}"
            );
        }
    }

    #[test]
    fn recurring_bit_is_fixedmeta_13_bit_02() {
        let mut s = fixture();
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1]
                .fields
                .as_ref()
                .unwrap()
                .recurring,
            Some(false)
        );
        s.fm[16 + 4 * 47 + 13] |= 0x02;
        let bytes = file(&s, true);
        assert_eq!(
            decode(&bytes).unwrap()[1]
                .fields
                .as_ref()
                .unwrap()
                .recurring,
            Some(true)
        );
        let imported = crate::project::project_from_mpp(&bytes).unwrap();
        assert_eq!(imported.tasks[0].recurring, Some(true));
        let reread = projcore::mspdi::read_mspdi(&projcore::mspdi::write_mspdi(&imported)).unwrap();
        assert_eq!(reread.tasks[0].recurring, Some(true));
    }

    #[test]
    fn task_field_offsets_decode_defaults_and_changes() {
        let mut s = fixture();
        let base = decode(&file(&s, true)).unwrap().remove(1).fields.unwrap();
        assert_eq!(base.task_type, Some(projcore::TaskType::FixedUnits));
        assert_eq!(base.priority, Some(0));
        assert_eq!(base.leveling_delay, Some(0));
        assert_eq!(base.leveling_delay_format, Some(8));
        assert_eq!(base.estimated, Some(false));
        assert_eq!(base.deadline.as_deref(), Some("1983-12-31 00:00"));
        assert_eq!(
            base.guid.as_deref(),
            Some("00000004-0000-0000-0000-000000000000")
        );
        assert!(base.create_date.is_some());
        let d = 250;
        s.fd[d + 70..d + 74].copy_from_slice(&600u32.to_le_bytes());
        s.fd[d + 78..d + 80].copy_from_slice(&123u16.to_le_bytes());
        s.fd[d + 140..d + 142].copy_from_slice(&2u16.to_le_bytes());
        s.fd[d + 162..d + 164].copy_from_slice(&6u16.to_le_bytes());
        s.fd[d + 164..d + 166].copy_from_slice(&32u16.to_le_bytes());
        s.fd[d + 128..d + 132].copy_from_slice(&[0xc0, 0x12, 0x86, 0x3a]);
        s.f2d[4 * 64] = 42;
        let created = s.fd[d + 128..d + 132].to_vec();
        s.fd[d + 182..d + 186].copy_from_slice(&created);
        let changed = decode(&file(&s, true)).unwrap().remove(1).fields.unwrap();
        assert_eq!(changed.task_type, Some(projcore::TaskType::FixedWork));
        assert_eq!(changed.priority, Some(123));
        assert_eq!(changed.leveling_delay, Some(600));
        assert_eq!(changed.leveling_delay_format, Some(6));
        assert_eq!(changed.estimated, Some(true));
        assert_eq!(
            changed.guid.as_deref(),
            Some("0000002A-0000-0000-0000-000000000000")
        );
        assert_ne!(changed.create_date, base.create_date);
        assert_eq!(changed.deadline, changed.create_date);
        s.fd[d + 162..d + 164].copy_from_slice(&40u16.to_le_bytes());
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1]
                .fields
                .as_ref()
                .unwrap()
                .leveling_delay_format,
            Some(40)
        );
        s.fd[d + 162..d + 164].copy_from_slice(&99u16.to_le_bytes());
        reject(&s);
    }

    #[test]
    fn earned_value_method_needs_fixed2_bit_without_fixedmeta_override() {
        let mut s = fixture();
        let f = 16 + 4 * 96;
        let m = 16 + 4 * 47;
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1]
                .fields
                .as_ref()
                .unwrap()
                .earned_value_method,
            Some(0)
        );
        s.f2m[f + 76] |= 0x80;
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1]
                .fields
                .as_ref()
                .unwrap()
                .earned_value_method,
            Some(1)
        );
        s.f2m[f + 76] |= 0x40; // also occurs on a real physical-EV task
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1]
                .fields
                .as_ref()
                .unwrap()
                .earned_value_method,
            Some(1)
        );
        s.f2m[16 + 3 * 96 + 76] = 0xc0; // UID 0 project summary
        assert_eq!(
            decode(&file(&s, true)).unwrap()[0]
                .fields
                .as_ref()
                .unwrap()
                .earned_value_method,
            Some(0)
        );
        s.f2m[f + 76] &= !0x40;
        s.fm[m + 26] |= 0x80;
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1]
                .fields
                .as_ref()
                .unwrap()
                .earned_value_method,
            Some(0)
        );
    }
    #[test]
    fn task_fields_reject_invalid_type_and_priority() {
        let mut s = fixture();
        s.fd[250 + 140..250 + 142].copy_from_slice(&3u16.to_le_bytes());
        assert!(decode(&file(&s, true)).unwrap_err().contains("task type"));
        s.fd[250 + 140..250 + 142].copy_from_slice(&0u16.to_le_bytes());
        s.fd[250 + 78..250 + 80].copy_from_slice(&1001u16.to_le_bytes());
        assert!(decode(&file(&s, true)).unwrap_err().contains("priority"));
    }
    #[test]
    fn refuses_structural_corruption() {
        let mut s = fixture();
        s.v2[4] = 1;
        reject(&s); // binary/control name
        let mut s = fixture();
        s.fd[250 + 0x68 + 2..250 + 0x68 + 4].copy_from_slice(&0xffffu16.to_le_bytes());
        reject(&s); // a task with no start date cannot be imported
        let mut s = fixture();
        s.fd[254..258].copy_from_slice(&0u32.to_le_bytes());
        reject(&s); // duplicate UID
        let mut s = fixture();
        s.fd[254..258].copy_from_slice(&u32::MAX.to_le_bytes());
        reject(&s); // UID cannot fit projcore
        let mut s = fixture();
        s.vm[24 + 12 + 4..24 + 12 + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        reject(&s); // bad offset
        let mut s = fixture();
        s.vm[24 + 12 + 4..24 + 12 + 8].copy_from_slice(&1u32.to_le_bytes());
        reject(&s); // reading at this mid-block offset gives an invalid length
        let mut s = fixture();
        s.fd.truncate(249);
        reject(&s); // last FixedMeta offset beyond truncated FixedData
        let mut s = fixture();
        s.fm[8..12].copy_from_slice(&6u32.to_le_bytes());
        reject(&s); // count overrun
        let mut s = fixture();
        s.vm[24 + 12..24 + 12 + 4].copy_from_slice(&0u32.to_le_bytes());
        reject(&s); // duplicate key
        let s = fixture();
        assert!(decode(&file(&s, false)).is_err()); // partial stream set
        let mut s = fixture();
        s.cons = link(9, 1, 7);
        reject(&s); // unknown predecessor
        for format in [0, 20, 21, 52, 53, 99] {
            let mut s = fixture();
            s.cons = link(1, 1, format);
            reject(&s); // lag format projcore cannot schedule
        }
        let mut s = fixture();
        s.cons = link(0, 1, 7);
        reject(&s); // project summary cannot be a link endpoint
    }
    /// Make FixedMeta entry 4 (task B, UID 1) a manual task: 2026-03-02 08:00
    /// to 2026-03-03 17:00, 16 hours shown in days (DurationFormat 7).
    fn make_manual(s: &mut Streams) {
        s.f2m[16 + 4 * 96 + 8] |= 0x80;
        let r = 4 * 64;
        s.f2d[r + 50..r + 54].copy_from_slice(&[0xc0, 0x12, 0x2a, 0x3c]);
        s.f2d[r + 54..r + 58].copy_from_slice(&[0xd8, 0x27, 0x2b, 0x3c]);
        s.f2d[r + 58..r + 62].copy_from_slice(&9600u32.to_le_bytes());
        s.f2d[r + 62..r + 64].copy_from_slice(&7u16.to_le_bytes());
    }
    #[test]
    fn manual_mode_and_fields_come_from_the_second_fixed_block() {
        let mut s = fixture();
        // Manual bytes on an auto task are stale, not its manual fields.
        s.f2d[4 * 64 + 50..4 * 64 + 64].fill(0x11);
        let tasks = decode(&file(&s, true)).unwrap();
        assert!(!tasks[1].manual);
        assert_eq!(tasks[1].manual_start, None);
        assert_eq!(tasks[1].manual_duration_min, None);
        make_manual(&mut s);
        let tasks = decode(&file(&s, true)).unwrap();
        assert!(!tasks[0].manual);
        let b = &tasks[1];
        assert!(b.manual);
        assert_eq!(b.manual_start.as_deref(), Some("2026-03-02 08:00"));
        assert_eq!(b.manual_finish.as_deref(), Some("2026-03-03 17:00"));
        assert_eq!(b.manual_duration_min, Some(960));
        let r = 4 * 64;
        s.f2d[r + 62..r + 64].copy_from_slice(&39u16.to_le_bytes()); // estimated days
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1].manual_duration_min,
            Some(960)
        );
        s.f2d[r + 62..r + 64].copy_from_slice(&8u16.to_le_bytes()); // elapsed days
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1].manual_duration_min,
            None
        );
        s.f2d[r + 62..r + 64].copy_from_slice(&7u16.to_le_bytes());
        s.f2d[r + 58..r + 62].fill(0xff); // no stored duration
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1].manual_duration_min,
            None
        );
    }
    #[test]
    fn refuses_a_second_fixed_block_that_does_not_match_the_tasks() {
        let r = 4 * 64;
        let manual = || {
            let mut s = fixture();
            make_manual(&mut s);
            s
        };
        assert!(decode(&file(&manual(), true)).is_ok());
        let mut s = manual();
        s.f2d[r + 62..r + 64].copy_from_slice(&2u16.to_le_bytes());
        reject(&s); // unknown DurationFormat
        let mut s = manual();
        s.f2d[r + 54..r + 58].copy_from_slice(&[0xc0, 0x12, 0x29, 0x3c]);
        reject(&s); // manual finish before manual start
        let mut s = fixture();
        s.f2d[r + 16..r + 24].copy_from_slice(&1f64.to_le_bytes());
        reject(&s); // sort keys out of task ID order: records misaligned
        let mut s = fixture();
        s.f2d[r..r + 16].fill(0);
        reject(&s); // a task record without its GUID
        let mut s = fixture();
        s.f2m[8..12].copy_from_slice(&4u32.to_le_bytes());
        reject(&s); // Fixed2Meta counts a different number of entries
        let mut s = fixture();
        s.f2d.truncate(4 * 64);
        s.f2m[12..16].copy_from_slice(&(4 * 64u32).to_le_bytes());
        reject(&s); // Fixed2Data shorter than its entries
        let mut s = fixture();
        s.f2m[16 + 4 * 96 + 4..16 + 4 * 96 + 8].copy_from_slice(&(3 * 64u32).to_le_bytes());
        reject(&s); // entry offsets must step by one record
        let mut s = fixture();
        s.f2m.clear();
        reject(&s); // a current layout without Fixed2Meta is partial
    }
    #[test]
    fn reads_the_new_task_default_from_project_props() {
        let mut s = fixture();
        assert_eq!(new_tasks_are_manual(&file(&s, true)), Ok(false));
        s.props = props(&[0xff, 0]);
        assert_eq!(new_tasks_are_manual(&file(&s, true)), Ok(true));
        let project = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        assert!(project.new_tasks_are_manual);
        s.props = props(&[1, 0]);
        assert_eq!(new_tasks_are_manual(&file(&s, true)), Ok(true));
        let project = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        assert!(project.new_tasks_are_manual);
        s.props = props(&[1, 1]);
        assert!(new_tasks_are_manual(&file(&s, true)).is_err());
        assert!(crate::project::project_from_mpp(&file(&s, true)).is_err());
        s.props = crate::props::stream(&[]);
        assert!(new_tasks_are_manual(&file(&s, true)).is_err());
        // A file without a task table has no default to read.
        let no_tasks = write_cfb_tree(&[Node::Stream("Props", vec![0u8; 4])]);
        assert_eq!(new_tasks_are_manual(&no_tasks), Ok(false));
    }
    #[test]
    fn the_default_is_refused_with_a_task_table_that_is_refused() {
        let s = fixture();
        // Task streams without FixedMeta are a partial set, not "no table".
        let partial = write_cfb_tree(&[Node::Storage(
            "   114",
            vec![
                Node::Stream("Props", s.props.clone()),
                Node::Storage("TBkndTask", vec![Node::Stream("VarMeta", s.vm.clone())]),
            ],
        )]);
        assert!(decode(&partial).is_err());
        assert!(new_tasks_are_manual(&partial).is_err());
        // A readable default beside a refused table is not answered either.
        let mut s = fixture();
        s.v2[4] = 1; // control character in a task name
        assert!(decode(&file(&s, true)).is_err());
        assert!(new_tasks_are_manual(&file(&s, true)).is_err());
        let mut s = fixture();
        s.f2m.clear();
        assert!(new_tasks_are_manual(&file(&s, true)).is_err());
    }
    #[test]
    fn manual_leaf_imports_pinned_by_its_dates_and_auto_leaf_by_a_constraint() {
        use projcore::ConstraintType;
        let mut s = fixture();
        let auto = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        let task = &auto.tasks[0];
        assert!(!task.manual);
        assert_eq!(task.constraint, ConstraintType::MustStartOn);
        assert_eq!(task.pinned_dates(), None);
        make_manual(&mut s);
        let manual = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        let task = &manual.tasks[0];
        assert!(task.manual);
        assert_eq!(task.constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(task.constraint_date, None);
        assert_eq!(task.duration_min, 960);
        assert_eq!(task.manual_duration_min, Some(960));
        let (start, finish) = task.pinned_dates().unwrap();
        assert_eq!(start.to_mspdi(), "2026-03-02T08:00:00");
        assert_eq!(finish.unwrap().to_mspdi(), "2026-03-03T17:00:00");
    }
    #[test]
    fn percent_elapsed_and_estimated_lags_decode_in_their_kind() {
        // #104: the lag i32 at +14 is MSPDI's LinkLag: the percentage itself
        // for format 19, tenths of a minute for time formats.
        for (lag, format, want) in [
            (50i32, 19, 50),
            (-25, 19, -25),
            (28800, 8, 2880),
            (-14400, 8, -1440),
            (100800, 42, 10080),
            (1800, 5, 180),
        ] {
            let mut s = fixture();
            s.cons = link(1, 1, format);
            s.cons[14..18].copy_from_slice(&lag.to_le_bytes());
            let tasks = decode(&file(&s, true)).unwrap();
            assert_eq!(
                tasks[1].predecessors,
                vec![MppPred {
                    pred_uid: 1,
                    kind: 1,
                    lag: want,
                    lag_format: format,
                }],
                "{lag} format {format}"
            );
        }
    }
    /// Add a keyed Var2Data block for `uid`.
    fn add_var(s: &mut Streams, uid: u32, key: u16, value: &[u8]) {
        let off = s.v2.len() as u32;
        s.v2.extend_from_slice(&(value.len() as u32).to_le_bytes());
        s.v2.extend_from_slice(value);
        s.vm.extend_from_slice(&uid.to_le_bytes());
        s.vm.extend_from_slice(&off.to_le_bytes());
        s.vm.extend_from_slice(&key.to_le_bytes());
        s.vm.extend_from_slice(&0x0b40u16.to_le_bytes());
        let count = u32_at(&s.vm, 8) + 1;
        s.vm[8..12].copy_from_slice(&count.to_le_bytes());
        let len = s.v2.len() as u32;
        s.vm[20..24].copy_from_slice(&len.to_le_bytes());
    }
    /// Task B (UID 1, FixedData record at 250) a quarter done: started
    /// 2026-03-02 08:00, stopped 17:00, resuming the next morning.
    const B: usize = 250;
    fn put(s: &mut Streams, off: usize, bytes: &[u8]) {
        s.fd[B + off..B + off + bytes.len()].copy_from_slice(bytes);
    }
    fn make_progress(s: &mut Streams) {
        put(s, 8, &1_440_000f64.to_le_bytes()); // 24h of work
        put(s, 16, &360_000f64.to_le_bytes());
        put(s, 24, &1_080_000f64.to_le_bytes());
        put(s, 32, &120_000f64.to_le_bytes());
        put(s, 40, &30_000.000_000_000_004f64.to_le_bytes());
        put(s, 56, &5913.505f64.to_le_bytes());
        put(s, 80, &2327i32.to_le_bytes()); // 232.7 minutes
        put(s, 88, &9600i32.to_le_bytes());
        put(s, 92, &25u16.to_le_bytes());
        put(s, 94, &37u16.to_le_bytes());
        put(s, 120, &[0xc0, 0x12, 0x2a, 0x3c]);
        put(s, 124, &[0xff; 4]); // NA: not finished
        put(s, 132, &[0xc0, 0x12, 0x2b, 0x3c]);
        put(s, 136, &[0xd8, 0x27, 0x2a, 0x3c]);
        add_var(s, 1, PHYSICAL_PERCENT_KEY, &40u16.to_le_bytes());
    }
    #[test]
    fn stored_duration_and_format_control_auto_import() {
        let mut s = fixture();
        // Two working days, with one day's work stored by Project.
        put(&mut s, 0x68, &[0xc0, 0x12, 0x2a, 0x3c]); // Mon 08:00
        put(&mut s, 0x6c, &[0xd8, 0x27, 0x2b, 0x3c]); // Tue 17:00
        put(&mut s, NEWEST_PROGRESS.duration, &4800i32.to_le_bytes());
        put(&mut s, NEWEST_PROGRESS.duration_format, &7u16.to_le_bytes());
        put(&mut s, NEWEST_PROGRESS.calendar_uid, &3i32.to_le_bytes());
        let decoded = decode(&file(&s, true)).unwrap();
        assert_eq!(decoded[1].duration_min, Some(480));
        assert_eq!(decoded[1].duration_format, Some(7));
        assert_eq!(decoded[1].calendar_uid, Some(3));
        let imported = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        let task = &imported.tasks[0];
        // Without TBkndCal, the import keeps synthesized Standard.
        assert_eq!(task.calendar_uid, None);
        assert_eq!(task.duration_min, 480);
        assert_eq!(task.stored_start.unwrap().to_mspdi(), "2026-03-02T08:00:00");
        assert_eq!(
            task.stored_finish.unwrap().to_mspdi(),
            "2026-03-03T17:00:00"
        );

        // An elapsed format keeps the span: projcore cannot schedule elapsed duration.
        put(&mut s, NEWEST_PROGRESS.duration_format, &8u16.to_le_bytes());
        let elapsed = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        assert_eq!(elapsed.tasks[0].duration_min, 960);

        // An unknown format likewise keeps the span.
        put(
            &mut s,
            NEWEST_PROGRESS.duration_format,
            &21u16.to_le_bytes(),
        );
        let unknown = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        assert_eq!(unknown.tasks[0].duration_min, 960);
    }
    #[test]
    fn task_calendar_assignment_is_validated_and_imported() {
        let mut s = fixture();
        put(&mut s, NEWEST_PROGRESS.duration, &4800i32.to_le_bytes());
        put(&mut s, NEWEST_PROGRESS.duration_format, &7u16.to_le_bytes());
        put(&mut s, NEWEST_PROGRESS.calendar_uid, &5i32.to_le_bytes());
        let project =
            crate::project::project_from_mpp(&file_with_calendar(&s, false, &[])).unwrap();
        assert_eq!(project.tasks[0].calendar_uid, Some(5));
        assert_eq!(project.tasks[0].duration_min, 480);

        put(&mut s, NEWEST_PROGRESS.calendar_uid, &9i32.to_le_bytes());
        assert_eq!(
            crate::project::project_from_mpp(&file_with_calendar(&s, false, &[])).unwrap_err(),
            "unknown calendar UID 9 for task UID 1"
        );

        put(&mut s, NEWEST_PROGRESS.calendar_uid, &(-1i32).to_le_bytes());
        let project =
            crate::project::project_from_mpp(&file_with_calendar(&s, false, &[])).unwrap();
        assert_eq!(project.tasks[0].calendar_uid, None);

        put(&mut s, NEWEST_PROGRESS.calendar_uid, &5i32.to_le_bytes());
        let error =
            crate::project::project_from_mpp(&file_with_calendar(&s, true, &[])).unwrap_err();
        assert!(
            error.contains("calendar \"Alice\" (UID 5) has no working time"),
            "{error}"
        );
        assert!(
            error.contains("task \"B\" (UID 1) cannot be scheduled"),
            "{error}"
        );
    }
    #[test]
    fn calendars_with_work_weeks_keep_task_calendar_and_stored_duration() {
        let mut s = fixture();
        put(&mut s, 0x68, &[0xc0, 0x12, 0x2a, 0x3c]); // Mon 08:00
        put(&mut s, 0x6c, &[0xd8, 0x27, 0x2b, 0x3c]); // Tue 17:00
        put(&mut s, NEWEST_PROGRESS.duration, &4800i32.to_le_bytes());
        put(&mut s, NEWEST_PROGRESS.duration_format, &7u16.to_le_bytes());
        put(&mut s, NEWEST_PROGRESS.calendar_uid, &5i32.to_le_bytes());

        for weeks in [&[5][..], &[1][..]] {
            let project =
                crate::project::project_from_mpp(&file_with_calendar(&s, false, weeks)).unwrap();
            assert_eq!(project.tasks[0].calendar_uid, Some(5));
            assert_eq!(project.tasks[0].duration_min, 480);
            assert_eq!(project.calendar(weeks[0]).unwrap().work_weeks.len(), 1);
        }

        let mut project =
            crate::project::project_from_mpp(&file_with_calendar(&s, false, &[5])).unwrap();
        // The generic task fixture encodes an ActualStart at the MPP epoch;
        // clear that unrelated progress marker to exercise auto scheduling.
        project.tasks[0].actual_start = None;
        let result = projcore::schedule::schedule(&project);
        let task = result.get(1).unwrap();
        assert_eq!(task.early_start, project.tasks[0].stored_start.unwrap());
        assert_eq!(task.early_finish.to_mspdi(), "2026-03-03T08:00:00");
    }

    #[test]
    fn closed_default_week_is_refused_even_with_an_open_work_week() {
        let mut s = fixture();
        put(&mut s, NEWEST_PROGRESS.calendar_uid, &5i32.to_le_bytes());
        let error =
            crate::project::project_from_mpp(&file_with_calendar(&s, true, &[5])).unwrap_err();
        assert!(
            error.contains("calendar \"Alice\" (UID 5) has no working time"),
            "{error}"
        );
    }
    #[test]
    fn negative_stored_duration_names_uid() {
        let mut s = fixture();
        put(&mut s, NEWEST_PROGRESS.duration, &(-1i32).to_le_bytes());
        assert_eq!(
            decode(&file(&s, true)).unwrap_err(),
            "negative duration for UID 1"
        );
    }
    #[test]
    fn progress_decodes_in_mspdi_units_and_imports_as_read() {
        let mut s = fixture();
        make_progress(&mut s);
        let tasks = decode(&file(&s, true)).unwrap();
        let rate = |text| Rate::parse(text).unwrap();
        assert_eq!(
            tasks[1].progress,
            Some(MppProgress {
                percent_complete: 25,
                percent_work_complete: 37,
                physical_percent_complete: 40,
                actual_start: Some("2026-03-02 08:00".into()),
                actual_finish: None,
                stop: Some("2026-03-02 17:00".into()),
                resume: Some("2026-03-03 08:00".into()),
                actual_duration_min: 233,
                remaining_duration_min: 960,
                work_min: 1440,
                actual_work_min: 360,
                remaining_work_min: 1080,
                cost: rate("120000"),
                actual_cost: rate("30000"),
                remaining_cost: rate("5913.51"),
            })
        );
        // A task without a physical-percent block has none.
        assert_eq!(
            tasks[0]
                .progress
                .as_ref()
                .unwrap()
                .physical_percent_complete,
            0
        );
        let project = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        let t = &project.tasks[0];
        let dt = |d: Option<projcore::DateTime>| d.map(|d| d.to_mspdi());
        assert_eq!(
            (
                t.percent_complete,
                t.percent_work_complete,
                t.physical_percent_complete
            ),
            (Some(25), Some(37), Some(40))
        );
        assert_eq!(dt(t.actual_start).as_deref(), Some("2026-03-02T08:00:00"));
        assert_eq!(t.actual_finish, None);
        assert_eq!(dt(t.stop).as_deref(), Some("2026-03-02T17:00:00"));
        assert_eq!(dt(t.resume).as_deref(), Some("2026-03-03T08:00:00"));
        assert_eq!(
            (t.actual_duration_min, t.remaining_duration_min),
            (Some(233), Some(960))
        );
        assert_eq!(
            (t.work_min, t.actual_work_min, t.remaining_work_min),
            (Some(1440), Some(360), Some(1080))
        );
        assert_eq!(
            (&t.cost, &t.actual_cost, &t.remaining_cost),
            (
                &Some(rate("120000")),
                &Some(rate("30000")),
                &Some(rate("5913.51"))
            )
        );
        assert_eq!((t.start_variance, t.finish_variance), (None, None));
        assert_eq!(t.work_variance, None);
        // Progress is kept, not scheduled from: the leaf stays pinned.
        assert_eq!(t.constraint, projcore::ConstraintType::MustStartOn);
    }
    #[test]
    fn refuses_progress_outside_its_range() {
        let refused = |edit: &dyn Fn(&mut Streams)| {
            let mut s = fixture();
            make_progress(&mut s);
            assert!(decode(&file(&s, true)).is_ok());
            edit(&mut s);
            reject(&s);
        };
        refused(&|s| put(s, 92, &101u16.to_le_bytes()));
        refused(&|s| put(s, 94, &0xffffu16.to_le_bytes()));
        refused(&|s| put(s, 80, &(-10i32).to_le_bytes()));
        refused(&|s| put(s, 16, &f64::NAN.to_le_bytes()));
        refused(&|s| put(s, 24, &(-1f64).to_le_bytes()));
        refused(&|s| put(s, 40, &f64::INFINITY.to_le_bytes()));
        // An actual finish before the actual start, or without one.
        refused(&|s| put(s, 124, &[0xd8, 0x27, 0x29, 0x3c]));
        let mut s = fixture();
        make_progress(&mut s);
        put(&mut s, 120, &[0xff; 4]);
        put(&mut s, 124, &[0xd8, 0x27, 0x2a, 0x3c]);
        reject(&s);
        // Physical percent complete: a 2-byte percentage.
        let mut s = fixture();
        add_var(&mut s, 1, PHYSICAL_PERCENT_KEY, &101u16.to_le_bytes());
        reject(&s);
        let mut s = fixture();
        add_var(&mut s, 1, PHYSICAL_PERCENT_KEY, &40u32.to_le_bytes());
        reject(&s);
    }
    #[test]
    fn costs_round_to_cents_as_project_writes_them() {
        for (value, text) in [
            (0.0, "0"),
            (-0.0, "0"),
            (100_000.0, "100000"),
            (9319.5, "9319.5"),
            (3405.9950000000003, "3406"),
            (348.315, "348.31"),
            (707.185, "707.18"),
            (5913.505, "5913.51"),
            (-12.5, "-12.5"),
        ] {
            assert_eq!(cost_rate(value).unwrap().as_str(), text, "{value}");
        }
        assert_eq!(cost_rate(f64::NAN), None);
    }
    fn link(pred: u32, succ: u32, format: u16) -> Vec<u8> {
        let mut r = vec![0u8; 20];
        r[4..8].copy_from_slice(&pred.to_le_bytes());
        r[8..12].copy_from_slice(&succ.to_le_bytes());
        r[12..14].copy_from_slice(&1u16.to_le_bytes());
        r[18..20].copy_from_slice(&format.to_le_bytes());
        r
    }
}
