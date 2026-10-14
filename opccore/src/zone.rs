//! The security zone Windows records for a downloaded file: its
//! `Zone.Identifier` alternate data stream (the "Mark of the Web"). A file
//! from the Internet (zone 3) or a Restricted site (zone 4) opens in
//! Protected View (#610, #894). Shared by the suite and terminal xlsxy, so
//! both read the stream's encodings the same way.

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

/// The path of `path`'s `Zone.Identifier` stream, as given (never `\\?\`).
#[cfg(windows)]
pub fn zone_stream(path: &Path) -> PathBuf {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    PathBuf::from(stream)
}

/// The Internet (3) and Restricted (4) zones open in Protected View.
pub fn is_protected_zone(zone: Option<u32>) -> bool {
    matches!(zone, Some(3 | 4))
}

/// The security zone Windows recorded for `path` when it was downloaded:
/// its `Zone.Identifier` alternate data stream. `None` when there is none,
/// and always off Windows. The stream is read on the path as given, never a
/// canonical `\\?\` form.
pub fn zone_id(path: &Path) -> Option<u32> {
    #[cfg(windows)]
    {
        std::fs::read(zone_stream(path))
            .ok()
            .and_then(|bytes| parse_zone_identifier(&bytes))
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

/// `ZoneId=` under `[ZoneTransfer]` in a `Zone.Identifier` stream. Browsers
/// write it as UTF-8 (sometimes with a BOM), some tools as UTF-16LE with a
/// BOM; lines may end in CRLF, and keys may carry spaces.
pub fn parse_zone_identifier(bytes: &[u8]) -> Option<u32> {
    let text = match bytes {
        [0xFF, 0xFE, rest @ ..] => {
            // An odd trailing byte is not a code unit, and is ignored.
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_le_bytes(pair))
                .collect();
            String::from_utf16_lossy(&units)
        }
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    };
    let mut in_transfer = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_transfer = line.eq_ignore_ascii_case("[ZoneTransfer]");
            continue;
        }
        if !in_transfer {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("ZoneId")
        {
            return value.trim().parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_zone_identifier_internet_and_restricted() {
        assert_eq!(
            parse_zone_identifier(b"[ZoneTransfer]\r\nZoneId=3\r\n"),
            Some(3)
        );
        assert_eq!(parse_zone_identifier(b"[ZoneTransfer]\nZoneId=4"), Some(4));
        assert!(is_protected_zone(Some(3)));
        assert!(is_protected_zone(Some(4)));
    }

    #[test]
    fn parse_zone_identifier_local_zones_are_not_protected() {
        for z in 0..=2 {
            let text = format!("[ZoneTransfer]\r\nZoneId={z}\r\n");
            assert_eq!(parse_zone_identifier(text.as_bytes()), Some(z));
            assert!(!is_protected_zone(Some(z)));
        }
        assert!(!is_protected_zone(None));
    }

    #[test]
    fn parse_zone_identifier_reads_utf16le_with_bom() {
        let text = "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/b.xlsx\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(parse_zone_identifier(&bytes), Some(3));
        // An odd trailing byte is no code unit and changes nothing.
        let mut odd = bytes.clone();
        odd.push(b'Z');
        assert_eq!(parse_zone_identifier(&odd), Some(3));
        let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
        utf8_bom.extend_from_slice(text.as_bytes());
        assert_eq!(parse_zone_identifier(&utf8_bom), Some(3));
    }

    #[test]
    fn parse_zone_identifier_tolerates_whitespace_and_other_keys() {
        let text = b"  [ZoneTransfer]  \r\n ReferrerUrl=https://x \r\n  ZoneId = 3  \r\n";
        assert_eq!(parse_zone_identifier(text), Some(3));
    }

    #[test]
    fn parse_zone_identifier_garbage_is_none() {
        assert_eq!(parse_zone_identifier(b""), None);
        assert_eq!(parse_zone_identifier(b"\x00\x01garbage"), None);
        assert_eq!(parse_zone_identifier(b"[ZoneTransfer]\r\nZoneId=x"), None);
        // ZoneId outside its section does not count.
        assert_eq!(parse_zone_identifier(b"[Other]\r\nZoneId=3"), None);
    }

    #[cfg(windows)]
    #[test]
    fn zone_id_reads_the_alternate_data_stream() {
        let dir = std::env::temp_dir().join(format!("opccore-zone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("book.xlsx");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(zone_id(&file), None);
        let marked = std::fs::write(zone_stream(&file), b"[ZoneTransfer]\r\nZoneId=3\r\n");
        if let Err(e) = marked {
            let _ = std::fs::remove_dir_all(&dir);
            eprintln!("SKIP zone_id_reads_the_alternate_data_stream: no ADS here ({e})");
            return;
        }
        assert_eq!(zone_id(&file), Some(3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn zone_id_is_none_off_windows() {
        assert_eq!(zone_id(Path::new("Cargo.toml")), None);
    }
}
