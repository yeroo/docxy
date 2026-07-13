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
/// recognized — its start and finish (`YYYY-MM-DD HH:MM`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MppTask {
    pub name: String,
    pub start: Option<String>,
    pub finish: Option<String>,
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
/// starts vary — a strong self-validating fit that generalizes across MPP9 and
/// MPP12/14 (verified on real Microsoft Project and ProjectLibre files). If no
/// layout fits, dates are left `None`. Timestamps use MPXJ's epoch/encoding.
pub fn tasks(bytes: &[u8]) -> Vec<MppTask> {
    let Ok(cfb) = Cfb::open(bytes) else { return Vec::new() };
    let Some(v2path) = cfb.paths().into_iter().find(|p| p.ends_with("TBkndTask/Var2Data")) else {
        return Vec::new();
    };
    let vmpath = v2path.replace("Var2Data", "VarMeta");
    let (Some(v2), Some(vm)) = (cfb.read_path(&v2path), cfb.read_path(&vmpath)) else {
        return Vec::new();
    };
    let names = crate::varmeta::names(&vm, &v2);
    let mut out: Vec<MppTask> = names.into_iter().map(|name| MppTask { name, ..Default::default() }).collect();

    // Add dates from FixedData if a record layout fits the task count.
    let fdpath = v2path.replace("Var2Data", "FixedData");
    if let Some(fd) = cfb.read_path(&fdpath) {
        if let Some((rs, off)) = detect_date_layout(&fd, out.len()) {
            for (i, t) in out.iter_mut().enumerate() {
                t.start = decode_timestamp(&fd, i * rs + off);
                t.finish = decode_timestamp(&fd, i * rs + off + 4);
            }
        }
    }
    out
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
fn detect_date_layout(fd: &[u8], count: usize) -> Option<(usize, usize)> {
    if count < 3 {
        return None;
    }
    let mut best = (0usize, 0usize, 0usize);
    for rs in 80..320usize {
        if rs * (count - 1) + 8 > fd.len() {
            break;
        }
        for off in 0..rs.min(280) {
            let mut ok = 0usize;
            let mut distinct = std::collections::HashSet::new();
            for i in 0..count {
                if let (Some(s), Some(f)) = (ts_ord(fd, i * rs + off), ts_ord(fd, i * rs + off + 4)) {
                    if s <= f {
                        ok += 1;
                        distinct.insert(s);
                    }
                }
            }
            if ok > best.2 && distinct.len() >= 3 {
                best = (rs, off, ok);
            }
        }
    }
    (best.2 * 5 >= count * 4).then_some((best.0, best.1))
}

/// Open a `.mpp` (compound file) and extract its documented metadata.
pub fn read_mpp(bytes: &[u8]) -> Result<MppInfo, String> {
    let cfb = Cfb::open(bytes)?;
    let mut info = MppInfo { streams: cfb.stream_names(), ..MppInfo::default() };

    if let Some(sum) = cfb.read_stream(SUMMARY).and_then(|s| oleps::parse(&s)) {
        let s = |id| sum.get(&id).and_then(|p| p.as_str()).unwrap_or("").to_string();
        info.title = s(PID_TITLE);
        info.subject = s(PID_SUBJECT);
        info.author = s(PID_AUTHOR);
        info.keywords = s(PID_KEYWORDS);
        info.comments = s(PID_COMMENTS);
        info.last_author = s(PID_LAST_AUTHOR);
        info.revision = s(PID_REVISION);
        info.created = sum.get(&PID_CREATED).and_then(|p| p.as_time()).and_then(filetime_to_string);
        info.saved = sum.get(&PID_SAVED).and_then(|p| p.as_time()).and_then(filetime_to_string);
    }
    if let Some(doc) = cfb.read_stream(DOC_SUMMARY).and_then(|s| oleps::parse(&s)) {
        let s = |id| doc.get(&id).and_then(|p| p.as_str()).unwrap_or("").to_string();
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
    Some(format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, (secs % 3600) / 60))
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
        y, m, d, tod / 3600, (tod % 3600) / 60, tod % 60
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
        assert_eq!(decode_timestamp(&[0xC0, 0x12, 0x00, 0x00], 0).as_deref(), Some("1984-01-01 08:00"));
        // days=366 → 1985-01-01 (1984 is a leap year)
        assert_eq!(decode_timestamp(&[0x00, 0x00, 0x6E, 0x01], 0).as_deref(), Some("1985-01-01 00:00"));
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
        assert_eq!(detect_date_layout(&fd, starts.len()), Some((rs, off)));
        // Decode round-trips the first task's dates.
        assert_eq!(decode_timestamp(&fd, off).as_deref(), Some("1989-06-23 08:00"));
    }

    #[test]
    fn rejects_when_no_layout_fits() {
        // All-0xFF is the NA date marker everywhere → no valid start/finish.
        let fd = vec![0xFFu8; 2000];
        assert_eq!(detect_date_layout(&fd, 5), None);
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
        assert_eq!(filetime_to_string(11_644_473_600 * 10_000_000).as_deref(), Some("1970-01-01 00:00:00"));
    }
}

