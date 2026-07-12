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
    fn filetime_epoch_and_zero() {
        assert_eq!(filetime_to_string(0), None);
        // 1970-01-01 00:00:00
        assert_eq!(filetime_to_string(11_644_473_600 * 10_000_000).as_deref(), Some("1970-01-01 00:00:00"));
    }
}

