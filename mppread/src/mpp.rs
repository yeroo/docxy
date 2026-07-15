//! High-level `.mpp` reading: what can be decoded *exactly* today.
//!
//! A `.mpp` file's task/resource data lives in undocumented, version-specific
//! var-data blocks that only a real-file corpus can validate — that decoder is
//! a later layer. What is documented, and therefore decodable now, is the
//! file's **metadata**: the OLE property-set streams every compound file
//! carries. [`read_mpp`] opens the CFB container, decodes those, and reports the
//! project's title/author/company/dates plus the raw stream directory — the map
//! for the eventual task decoder.

use crate::cfb::Cfb;
use crate::oleps;
use std::collections::HashMap;

const SUMMARY: &str = "\u{5}SummaryInformation";
const DOC_SUMMARY: &str = "\u{5}DocumentSummaryInformation";

// SummaryInformation property ids.
const PID_TITLE: u32 = 2;
const PID_SUBJECT: u32 = 3;
const PID_AUTHOR: u32 = 4;
const PID_KEYWORDS: u32 = 5;
const PID_COMMENTS: u32 = 6;
const PID_LAST_AUTHOR: u32 = 8;
const PID_REVISION: u32 = 9;
const PID_CREATED: u32 = 12;
const PID_SAVED: u32 = 13;
// DocumentSummaryInformation property ids.
const PID_MANAGER: u32 = 14;
const PID_COMPANY: u32 = 15;

/// Documented metadata extracted from a `.mpp` (or any compound file). Empty
/// strings / `None` mean the property was absent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MppInfo {
    pub title: String,
    pub subject: String,
    pub author: String,
    pub keywords: String,
    pub comments: String,
    pub last_author: String,
    pub revision: String,
    pub manager: String,
    pub company: String,
    /// Creation / last-save time as `YYYY-MM-DD HH:MM:SS` (UTC), if present.
    pub created: Option<String>,
    pub saved: Option<String>,
    /// Every stream in the container, for orientation / future decoding.
    pub streams: Vec<String>,
}

/// A task decoded from a `.mpp`: its name, and — when the fixed-record layout is
/// recognized — its start and finish (`YYYY-MM-DD HH:MM`), 1-based outline level
/// (the WBS depth; 1 = top level), and predecessor links.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MppTask {
    pub name: String,
    pub start: Option<String>,
    pub finish: Option<String>,
    pub outline_level: Option<u32>,
    /// Predecessor links onto this task, when the link table decodes.
    pub predecessors: Vec<MppPred>,
}

/// A predecessor link: the 0-based **index** of the predecessor task in the same
/// [`tasks`] slice, and the link kind (MSPDI's code: 0 = FF, 1 = FS, 2 = SF,
/// 3 = SS). Lag isn't decoded yet (0 in both corpus files); it's always `0`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MppPred {
    pub pred: usize,
    pub kind: u8,
}

/// Decode the task **names** from a `.mpp`, in task order. Empty if the task
/// tables aren't present. Reads the documented VarMeta/Var2Data container.
pub fn task_names(bytes: &[u8]) -> Vec<String> {
    tasks(bytes).into_iter().map(|t| t.name).collect()
}

/// Decode the tasks (name + start/finish) from a `.mpp`, in task order.
///
/// Names come from the VarMeta/Var2Data container. Start/Finish come from the
/// per-task **FixedData** records: the record size and the date field offset are
/// auto-detected as the layout under which every task's start ≤ finish and the
/// starts vary — and, when a link table is present, under which the
/// Finish-to-Start links hold (the tie-breaker that distinguishes the true
/// Start/Finish pair from a look-alike baseline/actual date field). A strong
/// self-validating fit that generalizes across MPP9 and MPP12/14 (verified on
/// real Microsoft Project and ProjectLibre files). If no layout fits, dates are
/// left `None`. Timestamps use MPXJ's epoch/encoding.
pub fn tasks(bytes: &[u8]) -> Vec<MppTask> {
    let Ok(cfb) = Cfb::open(bytes) else {
        return Vec::new();
    };
    let Some(v2path) = cfb
        .paths()
        .into_iter()
        .find(|p| p.ends_with("TBkndTask/Var2Data"))
    else {
        return Vec::new();
    };
    let vmpath = v2path.replace("Var2Data", "VarMeta");
    let (Some(v2), Some(vm)) = (cfb.read_path(&v2path), cfb.read_path(&vmpath)) else {
        return Vec::new();
    };
    let names = crate::varmeta::names(&vm, &v2);
    let mut out: Vec<MppTask> = names
        .into_iter()
        .map(|name| MppTask {
            name,
            ..Default::default()
        })
        .collect();

    // Add dates from FixedData if a record layout fits the task count. Once the
    // record size is known, look for the outline-level column in the same
    // records (the WBS depth), so summaries and rollup come through too, and
    // decode the task links from the sibling `TBkndCons` table.
    let fdpath = v2path.replace("Var2Data", "FixedData");
    let conspath = v2path.replace("TBkndTask/Var2Data", "TBkndCons/FixedData");
    let cons = cfb.read_path(&conspath);
    // Predecessor→successor index pairs (uid==position assumption) — the oracle
    // that disambiguates which date field is Start/Finish.
    let naive_links: Vec<(usize, usize)> = cons
        .as_deref()
        .map(|c| {
            parse_cons(c)
                .iter()
                .map(|l| (l.pred_uid as usize, l.succ_uid as usize))
                .collect()
        })
        .unwrap_or_default();
    if let Some(fd) = cfb.read_path(&fdpath) {
        if let Some((rs, off)) = detect_date_layout(&fd, out.len(), &naive_links) {
            for (i, t) in out.iter_mut().enumerate() {
                t.start = decode_timestamp(&fd, i * rs + off);
                t.finish = decode_timestamp(&fd, i * rs + off + 4);
            }
            if let Some(oc) = detect_outline_column(&fd, rs, out.len()) {
                for (i, t) in out.iter_mut().enumerate() {
                    t.outline_level = Some(fd[i * rs + oc] as u32 + 1);
                }
            }
            if let Some(cons) = cons {
                decode_links(&cons, &fd, rs, &mut out);
            }
        }
    }
    out
}

fn u32le(b: &[u8], o: usize) -> u32 {
    if o + 4 <= b.len() {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    } else {
        u32::MAX
    }
}

/// A raw `TBkndCons` link record: two task **unique ids** and the link kind.
struct RawLink {
    pred_uid: u32,
    succ_uid: u32,
    kind: u16,
}

/// Parse the fixed 20-byte `TBkndCons` records: `[uid][pred_uid][succ_uid]
/// [kind:u16][…lag]`. The kind is read as a 16-bit low word, which matches both
/// the MPP9 layout (kind at `+12` as `u16`) and the MPP12/14 layout (kind at
/// `+12` as `u32`, same low word). Lag isn't decoded (0 in the corpus).
fn parse_cons(cons: &[u8]) -> Vec<RawLink> {
    (0..cons.len() / 20)
        .map(|i| {
            let o = i * 20;
            RawLink {
                pred_uid: u32le(cons, o + 4),
                succ_uid: u32le(cons, o + 8),
                kind: u16le(cons, o + 12),
            }
        })
        .collect()
}

/// True if a Finish-to-Start link is consistent with the decoded dates: the
/// successor starts on or after the predecessor finishes (compared by date, so
/// same-day boundaries pass). Missing dates don't count against the fit.
fn fs_consistent(pred: &MppTask, succ: &MppTask) -> bool {
    match (&pred.finish, &succ.start) {
        (Some(f), Some(s)) => s[..s.len().min(10)] >= f[..f.len().min(10)],
        _ => true,
    }
}

/// Decode task links from a `TBkndCons` table and attach them as predecessors.
///
/// The link records reference tasks by **unique id**, which is *not* always the
/// task's position (MPP9 numbers them 0,1,2,…, but MPP12/14 can be sparse, e.g.
/// 0,2,4,5,…). So the per-task uid column in the FixedData records is found the
/// self-validating way: the u32 column under which the most Finish-to-Start
/// links satisfy the date ordering (`fs_consistent`) is the uid map. Links are
/// attached only if a strong (≥90%) fit is found — otherwise the table is left
/// undecoded rather than inventing dependencies.
fn decode_links(cons: &[u8], fd: &[u8], rs: usize, tasks: &mut [MppTask]) {
    let count = tasks.len();
    let links = parse_cons(cons);
    let fs: Vec<&RawLink> = links.iter().filter(|l| l.kind == 1).collect();
    if fs.is_empty() || count < 2 {
        return;
    }
    // Find the uid column: the u32 offset maximizing FS date-consistency.
    let build = |off: usize| -> HashMap<u32, usize> {
        let mut m = HashMap::new();
        for i in 0..count {
            m.entry(u32le(fd, i * rs + off)).or_insert(i);
        }
        m
    };
    let mut best: Option<(usize, usize)> = None; // (offset, valid FS count)
    for off in 0..rs.saturating_sub(3) {
        if (count - 1) * rs + off + 4 > fd.len() {
            continue;
        }
        let map = build(off);
        if map.len() * 10 < count * 9 {
            continue; // uids must be (near-)distinct
        }
        let ok = fs
            .iter()
            .filter_map(|l| Some((*map.get(&l.pred_uid)?, *map.get(&l.succ_uid)?)))
            .filter(|&(p, s)| fs_consistent(&tasks[p], &tasks[s]))
            .count();
        if best.is_none_or(|(_, b)| ok > b) {
            best = Some((off, ok));
        }
    }
    let Some((off, ok)) = best else { return };
    if ok * 10 < fs.len() * 9 {
        return; // no column reproduces the schedule → don't guess
    }
    let map = build(off);
    for l in &links {
        let (Some(&p), Some(&s)) = (map.get(&l.pred_uid), map.get(&l.succ_uid)) else {
            continue;
        };
        if p != s && l.kind <= 3 {
            tasks[s].predecessors.push(MppPred {
                pred: p,
                kind: l.kind as u8,
            });
        }
    }
}

/// A timestamp's ordinal (minutes since the MPP epoch) if it's a plausible
/// project date — for fast, allocation-free layout probing.
fn ts_ord(fd: &[u8], o: usize) -> Option<u32> {
    let days = u16le(fd, o + 2);
    // ~1989..2104 in days-since-1984, excluding the 0xFFFF NA marker.
    if !(2000..44000).contains(&days) {
        return None;
    }
    let mins = u16le(fd, o) as u32 * 6 / 60;
    Some(days as u32 * 1440 + mins)
}

/// Find the `(record_size, date_offset)` under which the most tasks have a valid
/// `start ≤ finish` and starts are varied. Accepts only a strong (≥80%) fit.
///
/// A `.mpp` record can carry several date-like field pairs (Start/Finish, but
/// also baseline, actual, early/late, constraint dates), and more than one can
/// satisfy `start ≤ finish` — so `start ≤ finish` alone sometimes locks onto the
/// wrong pair (e.g. one whose finishes are years off). When the link table is
/// available, `links` (predecessor→successor **index** pairs, under the cheap
/// uid==position assumption) breaks the tie: the true Start/Finish pair is the
/// one under which the Finish-to-Start links hold. The link oracle is trusted
/// only when it clearly applies — at least half the links map in range and ≥90%
/// are consistent — otherwise this falls back to the plain most-valid pair (so
/// files with sparse uids, where uid≠position, are unaffected).
fn detect_date_layout(fd: &[u8], count: usize, links: &[(usize, usize)]) -> Option<(usize, usize)> {
    if count < 3 {
        return None;
    }
    let mut val_best = (0usize, 0usize, 0usize); // (rs, off, valid count)
    let mut lnk_best: Option<(usize, usize, usize)> = None; // (rs, off, FS-consistent count)
    for rs in 80..320usize {
        if rs * (count - 1) + 8 > fd.len() {
            break;
        }
        for off in 0..rs.min(280) {
            let mut ok = 0usize;
            let mut distinct = std::collections::HashSet::new();
            for i in 0..count {
                if let (Some(s), Some(f)) = (ts_ord(fd, i * rs + off), ts_ord(fd, i * rs + off + 4))
                {
                    if s <= f {
                        ok += 1;
                        distinct.insert(s);
                    }
                }
            }
            if ok * 5 < count * 4 || distinct.len() < 3 {
                continue; // only strong-validity offsets are candidates
            }
            if ok > val_best.2 {
                val_best = (rs, off, ok);
            }
            // Link-consistency score for this offset (successor start ≥ predecessor finish).
            let (mut fo, mut ft) = (0usize, 0usize);
            for &(p, s) in links {
                if p < count && s < count {
                    if let (Some(pf), Some(ss)) =
                        (ts_ord(fd, p * rs + off + 4), ts_ord(fd, s * rs + off))
                    {
                        ft += 1;
                        if ss >= pf {
                            fo += 1;
                        }
                    }
                }
            }
            if ft >= 3
                && ft * 2 >= links.len()
                && fo * 10 >= ft * 9
                && lnk_best.is_none_or(|(_, _, b)| fo > b)
            {
                lnk_best = Some((rs, off, fo));
            }
        }
    }
    if let Some((rs, off, _)) = lnk_best {
        return Some((rs, off));
    }
    (val_best.2 * 5 >= count * 4).then_some((val_best.0, val_best.1))
}

/// Find the byte column in `record_size`-byte FixedData records that holds the
/// **outline level** (0-based WBS depth). It's identified by MS Project's tree
/// rule, a strong self-validating signature: the first task is at the top
/// (value ≤ 1), depth increases by at most one per row (you can't jump from
/// level 1 straight to level 3), it stays ≥ the root, it's shallow (≤ 10), and —
/// unlike a monotonic id column — it **pops back up** at least once (a summary's
/// children end and the next sibling returns to a shallower level). Among
/// columns that qualify, the most tree-like (most pop-ups) wins. `None` when no
/// column fits — the WBS then stays flat rather than inventing a hierarchy.
fn detect_outline_column(fd: &[u8], record_size: usize, count: usize) -> Option<usize> {
    if count < 3 || record_size == 0 {
        return None;
    }
    let mut best: Option<(usize, usize)> = None; // (offset, pop-ups)
    for off in 0..record_size {
        if (count - 1) * record_size + off >= fd.len() {
            break;
        }
        let v: Vec<i32> = (0..count)
            .map(|i| fd[i * record_size + off] as i32)
            .collect();
        let maxv = *v.iter().max().unwrap();
        if !(2..=10).contains(&maxv) || v[0] > 1 {
            continue;
        }
        let (mut ok, mut pops) = (true, 0usize);
        for i in 1..count {
            if v[i] < v[0] || v[i] > v[i - 1] + 1 {
                ok = false;
                break;
            }
            if v[i] < v[i - 1] {
                pops += 1;
            }
        }
        let distinct = v.iter().collect::<std::collections::HashSet<_>>().len();
        if ok && pops >= 1 && distinct >= 3 && best.is_none_or(|(_, p)| pops > p) {
            best = Some((off, pops));
        }
    }
    best.map(|(off, _)| off)
}

/// Open a `.mpp` (compound file) and extract its documented metadata.
pub fn read_mpp(bytes: &[u8]) -> Result<MppInfo, String> {
    let cfb = Cfb::open(bytes)?;
    let mut info = MppInfo {
        streams: cfb.stream_names(),
        ..MppInfo::default()
    };

    if let Some(sum) = cfb.read_stream(SUMMARY).and_then(|s| oleps::parse(&s)) {
        let s = |id| {
            sum.get(&id)
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string()
        };
        info.title = s(PID_TITLE);
        info.subject = s(PID_SUBJECT);
        info.author = s(PID_AUTHOR);
        info.keywords = s(PID_KEYWORDS);
        info.comments = s(PID_COMMENTS);
        info.last_author = s(PID_LAST_AUTHOR);
        info.revision = s(PID_REVISION);
        info.created = sum
            .get(&PID_CREATED)
            .and_then(|p| p.as_time())
            .and_then(filetime_to_string);
        info.saved = sum
            .get(&PID_SAVED)
            .and_then(|p| p.as_time())
            .and_then(filetime_to_string);
    }
    if let Some(doc) = cfb.read_stream(DOC_SUMMARY).and_then(|s| oleps::parse(&s)) {
        let s = |id| {
            doc.get(&id)
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string()
        };
        info.manager = s(PID_MANAGER);
        info.company = s(PID_COMPANY);
    }
    Ok(info)
}

/// Days from the Unix epoch (1970-01-01) to the MS Project epoch (1984-01-01):
/// 14 years incl. leap days 1972/76/80.
const MPP_EPOCH_DAYS: i64 = 5113;

fn u16le(b: &[u8], o: usize) -> u16 {
    if o + 2 <= b.len() {
        u16::from_le_bytes([b[o], b[o + 1]])
    } else {
        0xFFFF
    }
}

/// Decode an MPP timestamp at `off`: a 2-byte time (tenths of a minute since
/// midnight) at `+0` and a 2-byte date (days since 1984-01-01) at `+2`, per
/// MPXJ's `MPPUtility.getTimestamp`. Returns `None` for the NA marker (0xFFFF
/// days). Format `YYYY-MM-DD HH:MM`.
pub fn decode_timestamp(data: &[u8], off: usize) -> Option<String> {
    let time = u16le(data, off);
    let days = u16le(data, off + 2);
    if days == 0xFFFF {
        return None;
    }
    let secs = if time == 0xFFFF { 0 } else { time as u32 * 6 }; // tenths-min → seconds
    let (y, m, d) = civil_from_days(MPP_EPOCH_DAYS + days as i64);
    Some(format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60
    ))
}

/// Decode an MPP duration (a 4-byte value in tenths of a minute) at `off` into
/// **working minutes**. Per MPXJ, the raw int / 600 is hours; ÷10 is minutes.
pub fn decode_duration_minutes(data: &[u8], off: usize) -> i64 {
    let raw = if off + 4 <= data.len() {
        i32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
    } else {
        0
    };
    raw as i64 / 10
}

/// FILETIME (100-ns ticks since 1601-01-01 UTC) → `YYYY-MM-DD HH:MM:SS`.
/// Returns `None` for a zero/pre-epoch value.
fn filetime_to_string(ft: u64) -> Option<String> {
    if ft == 0 {
        return None;
    }
    let secs_1601 = (ft / 10_000_000) as i64;
    let unix = secs_1601 - 11_644_473_600; // 1601-01-01 → 1970-01-01
    let days = unix.div_euclid(86_400);
    let tod = unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m,
        d,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    ))
}

/// Day-number (since 1970-01-01) → (year, month, day). Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfb::write_cfb;
    use crate::oleps::Prop;

    // rebuild a property-set stream (mirrors oleps test helper)
    fn summary_stream(props: &[(u32, Prop)]) -> Vec<u8> {
        let count = props.len();
        let index_len = 8 + count * 8;
        let (mut index, mut values) = (Vec::new(), Vec::new());
        for (id, p) in props {
            let off = index_len + values.len();
            index.extend_from_slice(&id.to_le_bytes());
            index.extend_from_slice(&(off as u32).to_le_bytes());
            match p {
                Prop::Str(s) => {
                    let mut b: Vec<u8> = s.bytes().collect();
                    b.push(0);
                    while !b.len().is_multiple_of(4) {
                        b.push(0);
                    }
                    values.extend_from_slice(&30u32.to_le_bytes());
                    values.extend_from_slice(&((s.len() + 1) as u32).to_le_bytes());
                    values.extend_from_slice(&b);
                }
                Prop::Time(t) => {
                    values.extend_from_slice(&64u32.to_le_bytes());
                    values.extend_from_slice(&t.to_le_bytes());
                }
                Prop::Int(i) => {
                    values.extend_from_slice(&3u32.to_le_bytes());
                    values.extend_from_slice(&(*i as u32).to_le_bytes());
                }
            }
        }
        let cb = 8 + index.len() + values.len();
        let mut sec = Vec::new();
        sec.extend_from_slice(&(cb as u32).to_le_bytes());
        sec.extend_from_slice(&(count as u32).to_le_bytes());
        sec.extend_from_slice(&index);
        sec.extend_from_slice(&values);
        let mut out = Vec::new();
        out.extend_from_slice(&0xFFFEu16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&48u32.to_le_bytes());
        out.extend_from_slice(&sec);
        out
    }

    #[test]
    fn reads_metadata_from_a_compound_file() {
        // 2010-01-01 00:00:00 UTC in FILETIME ticks.
        let ft: u64 = (1_262_304_000 + 11_644_473_600) * 10_000_000;
        let summary = summary_stream(&[
            (PID_TITLE, Prop::Str("Bridge Project".into())),
            (PID_AUTHOR, Prop::Str("A. Engineer".into())),
            (PID_CREATED, Prop::Time(ft)),
        ]);
        let docsum = summary_stream(&[(PID_COMPANY, Prop::Str("Acme Ltd".into()))]);
        let mpp = write_cfb(&[
            (SUMMARY, summary),
            (DOC_SUMMARY, docsum),
            ("Props", vec![0u8; 20]), // a stub of the (undecoded) task data
        ]);

        let info = read_mpp(&mpp).unwrap();
        assert_eq!(info.title, "Bridge Project");
        assert_eq!(info.author, "A. Engineer");
        assert_eq!(info.company, "Acme Ltd");
        assert_eq!(info.created.as_deref(), Some("2010-01-01 00:00:00"));
        assert!(info.streams.iter().any(|s| s == "Props"));
    }

    #[test]
    fn mpp_timestamp_decode() {
        // days=0, time=4800 tenths-of-min (=8h) → 1984-01-01 08:00
        assert_eq!(
            decode_timestamp(&[0xC0, 0x12, 0x00, 0x00], 0).as_deref(),
            Some("1984-01-01 08:00")
        );
        // days=366 → 1985-01-01 (1984 is a leap year)
        assert_eq!(
            decode_timestamp(&[0x00, 0x00, 0x6E, 0x01], 0).as_deref(),
            Some("1985-01-01 00:00")
        );
        // 0xFFFF days is the NA marker
        assert_eq!(decode_timestamp(&[0x00, 0x00, 0xFF, 0xFF], 0), None);
    }

    // Encode an MPP timestamp (time tenths-of-min @+0, days-since-1984 @+2).
    fn ts(days: u16, mins: u16) -> [u8; 4] {
        let tenths = mins * 10;
        let [t0, t1] = tenths.to_le_bytes();
        let [d0, d1] = days.to_le_bytes();
        [t0, t1, d0, d1]
    }

    #[test]
    fn detects_fixed_record_date_layout() {
        // 5 tasks in 100-byte records, start/finish at offset 0x20; day+2 apart.
        let rs = 100usize;
        let off = 0x20usize;
        let starts = [2000u16, 2010, 2025, 2040, 2055];
        let mut fd = vec![0u8; rs * starts.len()];
        for (i, &s) in starts.iter().enumerate() {
            fd[i * rs + off..i * rs + off + 4].copy_from_slice(&ts(s, 8 * 60));
            fd[i * rs + off + 4..i * rs + off + 8].copy_from_slice(&ts(s + 2, 17 * 60));
        }
        assert_eq!(detect_date_layout(&fd, starts.len(), &[]), Some((rs, off)));
        // Decode round-trips the first task's dates.
        assert_eq!(
            decode_timestamp(&fd, off).as_deref(),
            Some("1989-06-23 08:00")
        );
    }

    #[test]
    fn detects_outline_level_column() {
        // 6 tasks in 40-byte records; outline (0-based) at offset 0x10.
        let rs = 40usize;
        let oc = 0x10usize;
        let levels = [0u8, 1, 2, 2, 1, 2]; // root, child, 2 grandkids, sibling, its child
        let mut fd = vec![0u8; rs * levels.len()];
        for (i, &lv) in levels.iter().enumerate() {
            fd[i * rs] = i as u8; // a monotonic id column (must NOT be chosen)
            fd[i * rs + oc] = lv;
        }
        assert_eq!(detect_outline_column(&fd, rs, levels.len()), Some(oc));
    }

    #[test]
    fn no_outline_column_when_flat() {
        // A flat list (all one level) has no pop-ups → no column detected.
        let rs = 40usize;
        let fd = vec![0u8; rs * 5]; // all zero everywhere
        assert_eq!(detect_outline_column(&fd, rs, 5), None);
    }

    #[test]
    fn decodes_links_with_sparse_uids() {
        // Three tasks, sparse uids [10, 20, 30] at record offset 0 in 8-byte
        // records; A→B and B→C finish-to-start links (kind 1).
        let rs = 8usize;
        let uids = [10u32, 20, 30];
        let mut fd = vec![0u8; rs * uids.len()];
        for (i, &u) in uids.iter().enumerate() {
            fd[i * rs..i * rs + 4].copy_from_slice(&u.to_le_bytes());
        }
        let mut cons = Vec::new();
        for &(p, s) in &[(10u32, 20u32), (20, 30)] {
            cons.extend_from_slice(&1u32.to_le_bytes()); // link uid
            cons.extend_from_slice(&p.to_le_bytes());
            cons.extend_from_slice(&s.to_le_bytes());
            cons.extend_from_slice(&1u16.to_le_bytes()); // kind = FS
            cons.extend_from_slice(&[0u8; 6]); // flag + lag
        }
        let mut tasks = vec![
            MppTask {
                name: "A".into(),
                start: Some("2020-01-01 08:00".into()),
                finish: Some("2020-01-02 17:00".into()),
                ..Default::default()
            },
            MppTask {
                name: "B".into(),
                start: Some("2020-01-03 08:00".into()),
                finish: Some("2020-01-04 17:00".into()),
                ..Default::default()
            },
            MppTask {
                name: "C".into(),
                start: Some("2020-01-05 08:00".into()),
                finish: Some("2020-01-06 17:00".into()),
                ..Default::default()
            },
        ];
        decode_links(&cons, &fd, rs, &mut tasks);
        assert_eq!(tasks[0].predecessors, vec![]);
        assert_eq!(tasks[1].predecessors, vec![MppPred { pred: 0, kind: 1 }]);
        assert_eq!(tasks[2].predecessors, vec![MppPred { pred: 1, kind: 1 }]);
    }

    #[test]
    fn rejects_links_that_contradict_dates() {
        // A link whose "successor" starts before its "predecessor" finishes has
        // no uid column that fits → left undecoded.
        let rs = 8usize;
        let mut fd = vec![0u8; rs * 2];
        fd[0..4].copy_from_slice(&0u32.to_le_bytes());
        fd[rs..rs + 4].copy_from_slice(&1u32.to_le_bytes());
        let mut cons = Vec::new();
        cons.extend_from_slice(&1u32.to_le_bytes());
        cons.extend_from_slice(&0u32.to_le_bytes()); // pred uid 0
        cons.extend_from_slice(&1u32.to_le_bytes()); // succ uid 1
        cons.extend_from_slice(&1u16.to_le_bytes());
        cons.extend_from_slice(&[0u8; 6]);
        let mut tasks = vec![
            MppTask {
                name: "late".into(),
                start: Some("2020-02-01 08:00".into()),
                finish: Some("2020-02-10 17:00".into()),
                ..Default::default()
            },
            MppTask {
                name: "early".into(),
                start: Some("2020-01-01 08:00".into()),
                finish: Some("2020-01-02 17:00".into()),
                ..Default::default()
            },
        ];
        decode_links(&cons, &fd, rs, &mut tasks);
        assert!(tasks.iter().all(|t| t.predecessors.is_empty()));
    }

    #[test]
    fn link_oracle_breaks_date_field_tie() {
        // Two date-pair fields both satisfy start ≤ finish, but only the real
        // one is consistent with the links. The decoy sits at a *lower* offset,
        // so plain validity would wrongly pick it; the link oracle must override.
        let rs = 96usize; // within the detector's 80..320 record-size scan
        let (decoy, real) = (0x08usize, 0x30usize);
        let count = 5;
        let mut fd = vec![0u8; rs * count];
        for i in 0..count {
            let base = i * rs;
            let i = i as u16;
            // decoy: descending starts (chain links go backwards → inconsistent)
            fd[base + decoy..base + decoy + 4].copy_from_slice(&ts(13000 + (5 - i) * 30, 480));
            fd[base + decoy + 4..base + decoy + 8].copy_from_slice(&ts(13010 + (5 - i) * 30, 480));
            // real: ascending starts (chain links go forward → consistent)
            fd[base + real..base + real + 4].copy_from_slice(&ts(13000 + i * 30, 480));
            fd[base + real + 4..base + real + 8].copy_from_slice(&ts(13010 + i * 30, 480));
        }
        let links = [(0usize, 1usize), (1, 2), (2, 3), (3, 4)];
        // Without links, validity alone picks the lower (decoy) offset.
        assert_eq!(detect_date_layout(&fd, count, &[]), Some((rs, decoy)));
        // With links, the oracle selects the real Start/Finish field.
        assert_eq!(detect_date_layout(&fd, count, &links), Some((rs, real)));
    }

    #[test]
    fn rejects_when_no_layout_fits() {
        // All-0xFF is the NA date marker everywhere → no valid start/finish.
        let fd = vec![0xFFu8; 2000];
        assert_eq!(detect_date_layout(&fd, 5, &[]), None);
    }

    #[test]
    fn mpp_duration_decode() {
        // 4800 tenths-of-a-minute = 480 working minutes (1 day @ 8h)
        assert_eq!(decode_duration_minutes(&[0xC0, 0x12, 0x00, 0x00], 0), 480);
    }

    #[test]
    fn filetime_epoch_and_zero() {
        assert_eq!(filetime_to_string(0), None);
        // 1970-01-01 00:00:00
        assert_eq!(
            filetime_to_string(11_644_473_600 * 10_000_000).as_deref(),
            Some("1970-01-01 00:00:00")
        );
    }
}
