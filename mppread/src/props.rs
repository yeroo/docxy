//! Current Project's keyed project property stream (`114/Props`).
//!
//! A 16-byte header (the stream length less 4, twice; `0x10000`; the entry
//! count) and then counted entries: a `u32` size, `u32` key, `u32` attribute
//! and `size` bytes of value, padded to an even length. The entries must end
//! exactly at the end of the stream.
use std::collections::HashMap;

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// Keyed values of a validated Props stream.
pub(crate) fn entries(b: &[u8]) -> Result<HashMap<u32, &[u8]>, String> {
    let header_len = (b.len() as u64).checked_sub(4);
    if b.len() < 16
        || Some(u32_at(b, 0) as u64) != header_len
        || u32_at(b, 4) != u32_at(b, 0)
        || u32_at(b, 8) != 0x10000
    {
        return Err("invalid project Props header".into());
    }
    let count = u32_at(b, 12) as usize;
    let mut out = HashMap::new();
    let mut o = 16usize;
    for _ in 0..count {
        if o + 12 > b.len() {
            return Err("project Props entry out of range".into());
        }
        let size = u32_at(b, o) as usize;
        let key = u32_at(b, o + 4);
        let Some(end) = (o + 12).checked_add(size).filter(|&n| n <= b.len()) else {
            return Err("project Props value out of range".into());
        };
        if out.insert(key, &b[o + 12..end]).is_some() {
            return Err(format!("duplicate project Props key {key:#x}"));
        }
        o = end + size % 2;
    }
    if o != b.len() {
        return Err("project Props entries do not fill the stream".into());
    }
    Ok(out)
}

/// `NewTasksAreManual`: a 2-byte value, `0`, `1`, or `0x00ff`.
pub(crate) const NEW_TASKS_ARE_MANUAL: u32 = 0x0240_13c8;
const PROJECT_START: u32 = 0x0240_0002;

/// Project StartDate in the same four-byte format as task timestamps.
pub(crate) fn project_start(b: &[u8]) -> Result<Option<String>, String> {
    let entries = entries(b)?;
    let Some(raw) = entries.get(&PROJECT_START) else {
        return Ok(None);
    };
    if raw.len() != 4 {
        return Err("invalid project StartDate in Props".into());
    }
    crate::mpp::decode_checked_timestamp(raw, 0)
        .map_err(|_| "invalid project StartDate in Props".into())
}
/// WBS code mask. Project writes four zero bytes for its ordinary numeric
/// outline mask in the paired corpus; absent also means no custom mask.
const WBS_CODE_MASK: u32 = 0x0240_138b;

pub(crate) fn has_default_wbs_mask(b: &[u8]) -> Result<bool, String> {
    Ok(matches!(
        entries(b)?.get(&WBS_CODE_MASK),
        None | Some(&[0, 0, 0, 0])
    ))
}

/// Project's default base calendar name, observed changing from Standard to
/// Night in the paired c1 calendar probe.
const DEFAULT_CALENDAR_NAME: u32 = 0x0240_000e;

pub(crate) fn default_calendar_name(b: &[u8]) -> Result<Option<String>, String> {
    let entries = entries(b)?;
    let Some(raw) = entries.get(&DEFAULT_CALENDAR_NAME) else {
        return Ok(None);
    };
    if raw.len() < 4 || !raw.len().is_multiple_of(2) {
        return Err("invalid default calendar name in Props".into());
    }
    let units: Vec<_> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let end = units
        .iter()
        .position(|&u| u == 0)
        .ok_or("unterminated default calendar name")?;
    if end == 0 || units[end..].iter().any(|&u| u != 0) {
        return Err("invalid default calendar name padding".into());
    }
    let name =
        String::from_utf16(&units[..end]).map_err(|_| "invalid UTF-16 default calendar name")?;
    Ok(Some(name))
}

/// Inserted-project table (task-fields/f6-subprojects, two inserted plans):
/// a `u32` block length, a `u32` count, the `u32` end of an index of `u32`
/// entries whose low half is a block offset, then four entries per item: a
/// 20-byte header (type `1` at +16), the task UID, and two OLE File Monikers
/// (MS-OLEDS 2.3.7) for the path and the relative name. Project's XML
/// SubprojectName is the path moniker's Unicode extension. Other item types
/// have no oracle and make the table unknown.
const SUBPROJECTS: u32 = 0x0240_00a2;
const FILE_MONIKER: [u8; 16] = [3, 3, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];

fn file_moniker_path(b: &[u8], at: usize) -> Result<String, String> {
    let bad = || "unrecognized subproject file moniker".to_string();
    let u16_at = |o: usize| {
        b.get(o..o + 2)
            .map(|s| u16::from_le_bytes(s.try_into().unwrap()))
    };
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32_at(s, 0) as usize);
    if b.get(at..at + 16) != Some(&FILE_MONIKER[..]) || u16_at(at + 16) != Some(0) {
        return Err(bad());
    }
    let ansi_len = u32_at(at + 18).ok_or_else(bad)?;
    let tail = (at + 22).checked_add(ansi_len).ok_or_else(bad)?;
    if u16_at(tail) != Some(0xffff)
        || u16_at(tail + 2) != Some(0xdead)
        || b.get(tail + 4..tail + 24)
            .is_none_or(|r| r.iter().any(|&x| x != 0))
    {
        return Err(bad());
    }
    let size = u32_at(tail + 24).ok_or_else(bad)?;
    if size == 0 {
        // No extension: Windows omits it when the ANSI path needs no long
        // form (f6's relative-name monikers). The ANSI code page is the
        // writer's, so only an ASCII path is known.
        let ansi = &b[at + 22..tail];
        return match ansi.split_last() {
            Some((0, path))
                if !path.is_empty() && path.iter().all(|c| (0x20..0x7f).contains(c)) =>
            {
                Ok(String::from_utf8(path.to_vec()).unwrap())
            }
            _ => Err(bad()),
        };
    }
    let bytes = u32_at(tail + 28).ok_or_else(bad)?;
    let start = tail + 34;
    if size != bytes + 6
        || u16_at(tail + 32) != Some(3)
        || !bytes.is_multiple_of(2)
        || start + bytes > b.len()
    {
        return Err(bad());
    }
    let units: Vec<u16> = b[start..start + bytes]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    String::from_utf16(&units)
        .ok()
        .filter(|path| !path.is_empty() && !path.chars().any(char::is_control))
        .ok_or_else(bad)
}

/// SubprojectName by task UID; no table means no inserted projects. An item
/// whose path moniker cannot be read has no name; an unknown item type makes
/// the whole table unknown, since the index stride is then unknown too.
pub(crate) fn subproject_names(b: &[u8]) -> Result<HashMap<u32, String>, String> {
    let entries = entries(b)?;
    let Some(block) = entries.get(&SUBPROJECTS) else {
        return Ok(HashMap::new());
    };
    let bad = || "unrecognized subproject table".to_string();
    if block.len() < 12 || u32_at(block, 0) as usize != block.len() {
        return Err(bad());
    }
    let index_end = u32_at(block, 8) as usize;
    if index_end < 12 || index_end > block.len() || !(index_end - 12).is_multiple_of(16) {
        return Err(bad());
    }
    let offset = |i: usize| u32_at(block, 12 + i * 4) as usize & 0xffff;
    let mut out = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    for item in 0..(index_end - 12) / 16 {
        let [header, uid, path] = [0, 1, 2].map(|k| offset(item * 4 + k));
        if block.get(header + 16) != Some(&1) || uid + 4 > block.len() {
            return Err(bad());
        }
        let task_uid = u32_at(block, uid);
        if !seen.insert(task_uid) {
            return Err(format!("duplicate subproject for task UID {task_uid}"));
        }
        if let Ok(name) = file_moniker_path(block, path) {
            out.insert(task_uid, name);
        }
    }
    Ok(out)
}

pub(crate) fn new_tasks_are_manual(b: &[u8]) -> Result<bool, String> {
    match entries(b)?.get(&NEW_TASKS_ARE_MANUAL) {
        Some([0, 0]) => Ok(false),
        Some([1, 0] | [0xff, 0]) => Ok(true),
        Some(v) => Err(format!("unrecognized NewTasksAreManual value {v:02x?}")),
        None => Err("project Props has no NewTasksAreManual".into()),
    }
}

/// Build a Props stream, for tests.
#[cfg(test)]
pub(crate) fn stream(values: &[(u32, &[u8])]) -> Vec<u8> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_start_distinguishes_missing_na_valid_and_invalid_values() {
        assert_eq!(project_start(&stream(&[])), Ok(None));
        assert_eq!(
            project_start(&stream(&[(PROJECT_START, &[0xff; 4])])),
            Ok(None)
        );
        assert_eq!(
            project_start(&stream(&[(PROJECT_START, &[0xc0, 0x12, 0x86, 0x3a])])),
            Ok(Some("2025-01-06 08:00".into()))
        );
        for value in [&[0u8; 3][..], &[0x41, 0x38, 0x86, 0x3a][..]] {
            assert_eq!(
                project_start(&stream(&[(PROJECT_START, value)])),
                Err("invalid project StartDate in Props".into())
            );
        }
    }

    #[test]
    fn reads_the_new_task_default_and_refuses_what_it_does_not_know() {
        let odd: &[u8] = &[1, 2, 3];
        let off = stream(&[(1, odd), (NEW_TASKS_ARE_MANUAL, &[0, 0])]);
        assert_eq!(new_tasks_are_manual(&off), Ok(false));
        let on = stream(&[(NEW_TASKS_ARE_MANUAL, &[0xff, 0])]);
        assert_eq!(new_tasks_are_manual(&on), Ok(true));
        assert_eq!(
            new_tasks_are_manual(&stream(&[(NEW_TASKS_ARE_MANUAL, &[1, 0])])),
            Ok(true)
        );
        for value in [&[1, 1][..], &[2, 0], &[0, 1], &[1], &[1, 0, 0]] {
            assert!(
                new_tasks_are_manual(&stream(&[(NEW_TASKS_ARE_MANUAL, value)])).is_err(),
                "accepted {value:02x?}"
            );
        }
        assert!(new_tasks_are_manual(&stream(&[(1, &[0, 0])])).is_err());
        let mut short = on.clone();
        short.pop();
        assert!(new_tasks_are_manual(&short).is_err()); // header length disagrees
        let mut trailing = on.clone();
        trailing.extend_from_slice(&[0, 0]);
        let len = ((trailing.len() - 4) as u32).to_le_bytes();
        trailing[..4].copy_from_slice(&len);
        trailing[4..8].copy_from_slice(&len);
        assert!(new_tasks_are_manual(&trailing).is_err()); // bytes past the entries
        let dup = stream(&[
            (NEW_TASKS_ARE_MANUAL, &[0, 0]),
            (NEW_TASKS_ARE_MANUAL, &[0, 0]),
        ]);
        assert!(new_tasks_are_manual(&dup).is_err());
    }

    /// An OLE File Moniker as Project writes it: the ANSI (8.3) path, then
    /// the long Unicode path as the extension, or a zero size when the ANSI
    /// path is already the long one (`unicode` empty).
    fn moniker(ansi: &[u8], unicode: &str) -> Vec<u8> {
        let mut m = FILE_MONIKER.to_vec();
        m.extend_from_slice(&0u16.to_le_bytes());
        m.extend_from_slice(&(ansi.len() as u32 + 1).to_le_bytes());
        m.extend_from_slice(ansi);
        m.push(0);
        m.extend_from_slice(&[0xff, 0xff, 0xad, 0xde]);
        m.extend_from_slice(&[0; 20]);
        if unicode.is_empty() {
            m.extend_from_slice(&0u32.to_le_bytes());
            return m;
        }
        let wide: Vec<u8> = unicode.encode_utf16().flat_map(u16::to_le_bytes).collect();
        m.extend_from_slice(&(wide.len() as u32 + 6).to_le_bytes());
        m.extend_from_slice(&(wide.len() as u32).to_le_bytes());
        m.extend_from_slice(&3u16.to_le_bytes());
        m.extend_from_slice(&wide);
        m
    }

    /// The f6-subprojects table shape: one item per (task UID, ANSI path,
    /// Unicode path).
    fn subprojects(items: &[(u32, &[u8], &str)], kind: u8) -> Vec<u8> {
        let index_end = 12 + items.len() * 16;
        let mut index = Vec::new();
        let mut body = Vec::new();
        for &(uid, ansi, unicode) in items {
            let mut header = vec![0u8; 20];
            header[16] = kind;
            let parts = [
                header,
                uid.to_le_bytes().to_vec(),
                moniker(ansi, unicode),
                moniker(br"\name.mpp", ""),
            ];
            for part in parts {
                index.extend_from_slice(&((index_end + body.len()) as u32).to_le_bytes());
                body.extend(part);
            }
        }
        let mut block = Vec::new();
        block.extend_from_slice(&((index_end + body.len()) as u32).to_le_bytes());
        block.extend_from_slice(&9u32.to_le_bytes());
        block.extend_from_slice(&(index_end as u32).to_le_bytes());
        block.extend(index);
        block.extend(body);
        stream(&[(SUBPROJECTS, &block)])
    }

    #[test]
    fn subproject_names_read_the_long_path_of_each_inserted_project() {
        let short: &[u8] = br"C:\USERS\JRGEN~1\F6-CHI~1.MPP";
        let long = r"C:\Users\Jürgen\Планы\f6-child.mpp";
        let names = subproject_names(&subprojects(
            &[(2, short, long), (3, br"D:\PLANS\B.MPP", "")],
            1,
        ))
        .unwrap();
        assert_eq!(names.len(), 2);
        assert_eq!(names[&2], long);
        // No Unicode extension: the ANSI path is the long path.
        assert_eq!(names[&3], r"D:\PLANS\B.MPP");
        assert_eq!(subproject_names(&stream(&[])), Ok(HashMap::new()));
        // Item types other than f6's have no oracle.
        assert!(subproject_names(&subprojects(&[(2, short, long)], 3)).is_err());
        assert!(subproject_names(&subprojects(&[(2, short, long), (2, short, long)], 1)).is_err());
        // An unreadable path leaves only that item unnamed: a non-ASCII ANSI
        // path in an unknown code page, or a torn Unicode extension.
        let names = subproject_names(&subprojects(
            &[(2, b"C:\\J\xfcrgen.mpp", ""), (3, br"D:\B.MPP", "")],
            1,
        ))
        .unwrap();
        assert_eq!(names, HashMap::from([(3, r"D:\B.MPP".to_string())]));
        let mut torn = subprojects(&[(2, short, long), (3, br"D:\B.MPP", "")], 1);
        let wide: Vec<u8> = long.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let path = torn.windows(wide.len()).position(|w| w == wide).unwrap();
        // The size before the byte count and key: they no longer agree.
        torn[path - 10] ^= 1;
        assert_eq!(
            subproject_names(&torn),
            Ok(HashMap::from([(3, r"D:\B.MPP".to_string())]))
        );
    }

    #[test]
    fn recognizes_default_and_custom_wbs_masks() {
        assert_eq!(
            has_default_wbs_mask(&stream(&[(WBS_CODE_MASK, &[0, 0, 0, 0])])),
            Ok(true)
        );
        assert_eq!(has_default_wbs_mask(&stream(&[])), Ok(true));
        assert_eq!(
            has_default_wbs_mask(&stream(&[(WBS_CODE_MASK, &[1, 0, 0, 0])])),
            Ok(false)
        );
    }
}
