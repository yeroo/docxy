//! Read-only ZIP reader (stored + deflate), including ZIP64 archives.
//!
//! Ported from rust365 (`src/zip.rs`), plus ZIP64 reading (#1094) and unit
//! tests.

use crate::inflate::inflate_raw;

pub struct ZipEntry {
    pub name: String,
    pub method: u16,
    pub comp_size: u64,
    pub uncomp_size: u64,
    pub local_offset: u64,
}

const EOCD_SIG: u32 = 0x06054b50;
const EOCD64_SIG: u32 = 0x06064b50;
const EOCD64_LOCATOR_SIG: u32 = 0x07064b50;
const CENTRAL_SIG: u32 = 0x02014b50;
const LOCAL_SIG: u32 = 0x04034b50;
/// A 32-bit central directory field holding this value is in the entry's
/// ZIP64 extra field instead.
const ZIP64_SENTINEL: u32 = 0xFFFF_FFFF;
const ZIP64_EXTRA_ID: u16 = 0x0001;

fn rd16(p: &[u8]) -> u16 {
    p[0] as u16 | ((p[1] as u16) << 8)
}
fn rd32(p: &[u8]) -> u32 {
    p[0] as u32 | ((p[1] as u32) << 8) | ((p[2] as u32) << 16) | ((p[3] as u32) << 24)
}
fn rd64(p: &[u8]) -> u64 {
    rd32(p) as u64 | ((rd32(&p[4..]) as u64) << 32)
}

/// `len` bytes of `data` at `off`, or `None` when they are not all there.
fn span(data: &[u8], off: usize, len: usize) -> Option<&[u8]> {
    data.get(off..off.checked_add(len)?)
}

/// Output cap for inflating a deflate entry that declares `uncomp_size` bytes,
/// or `None` when that size does not fit in memory here. `inflate_raw` treats
/// a cap of 0 as unlimited, so an entry declaring 0 bytes gets a cap of 1: its
/// stream is cut off within one block step instead of inflating without bound,
/// and the size check in `extract` still rejects any non-empty output.
fn deflate_cap(uncomp_size: u64) -> Option<usize> {
    usize::try_from(uncomp_size).ok().map(|n| n.max(1))
}

/// The entry count and central directory offset of a ZIP64 archive, read
/// from the ZIP64 end record its locator (just before the end record at
/// `eocd`) points to. `Some(None)` when there is no locator. An ordinary
/// archive can hold the locator's bytes by chance (an entry comment ending
/// the central directory), so when the end record's own fields are not
/// ZIP64 sentinels, a locator inside the central directory or one that
/// points at no ZIP64 end record is not one either: `Some(None)` too. With
/// sentinel fields, a locator that leads nowhere is `None`.
fn zip64_end(data: &[u8], eocd: usize) -> Option<Option<(u64, u64)>> {
    let end = &data[eocd..];
    let (count, cd_size, cd_offset) = (rd16(&end[10..]), rd32(&end[12..]), rd32(&end[16..]));
    let sentinels = count == 0xFFFF || cd_size == ZIP64_SENTINEL || cd_offset == ZIP64_SENTINEL;
    let Some(at) = eocd.checked_sub(20) else {
        return Some(None);
    };
    let locator = &data[at..eocd];
    if rd32(locator) != EOCD64_LOCATOR_SIG {
        return Some(None);
    }
    if !sentinels && cd_offset as u64 + cd_size as u64 > at as u64 {
        return Some(None);
    }
    let record = usize::try_from(rd64(&locator[8..]))
        .ok()
        .and_then(|rec| span(data, rec, 56))
        .filter(|rec| rd32(rec) == EOCD64_SIG);
    match record {
        Some(rec) => Some(Some((rd64(&rec[32..]), rd64(&rec[48..])))),
        None if sentinels => None,
        None => Some(None),
    }
}

/// The 64-bit values of the fields a central directory entry marked with
/// [`ZIP64_SENTINEL`], from its ZIP64 extra field: they come in the order
/// uncompressed size, compressed size, local header offset, and only the
/// marked ones are present. With no ZIP64 field (or an extra block that ends
/// before one) the marked fields keep their 32-bit value, as other readers
/// keep it; `None` when the ZIP64 field is too short for the marked values.
fn zip64_values(extra: &[u8], marked: [bool; 3]) -> Option<[Option<u64>; 3]> {
    let mut i = 0;
    while let Some(head) = span(extra, i, 4) {
        let Some(body) = span(extra, i + 4, rd16(&head[2..]) as usize) else {
            break;
        };
        if rd16(head) == ZIP64_EXTRA_ID {
            let mut out = [None; 3];
            let mut at = 0;
            for (slot, marked) in out.iter_mut().zip(marked) {
                if marked {
                    *slot = Some(rd64(span(body, at, 8)?));
                    at += 8;
                }
            }
            return Some(out);
        }
        i += 4 + body.len();
    }
    Some([None; 3])
}

pub struct ZipArchive<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntry>,
}

impl<'a> ZipArchive<'a> {
    pub fn open(data: &'a [u8]) -> Option<ZipArchive<'a>> {
        let size = data.len();
        if size < 22 {
            return None;
        }
        let max_back = size.min(22 + 65535);
        let stop = size - max_back;
        let mut off = size - 22;
        loop {
            if rd32(&data[off..]) == EOCD_SIG {
                break;
            }
            if off == stop {
                return None;
            }
            off -= 1;
        }
        let (count, cd_offset) = match zip64_end(data, off)? {
            Some(end) => end,
            None => (
                rd16(&data[off + 10..]) as u64,
                rd32(&data[off + 16..]) as u64,
            ),
        };
        let mut p = usize::try_from(cd_offset).ok()?;
        // Every entry takes at least 46 bytes, so a count the data cannot
        // hold fails below; do not reserve for it.
        let mut entries = Vec::with_capacity(usize::try_from(count).ok()?.min(size / 46));
        for _ in 0..count {
            let head = span(data, p, 46)?;
            if rd32(head) != CENTRAL_SIG {
                return None;
            }
            let method = rd16(&head[10..]);
            let comp_size = rd32(&head[20..]);
            let uncomp_size = rd32(&head[24..]);
            let name_len = rd16(&head[28..]) as usize;
            let extra_len = rd16(&head[30..]) as usize;
            let comment_len = rd16(&head[32..]) as usize;
            let local_offset = rd32(&head[42..]);
            let name = String::from_utf8_lossy(span(data, p + 46, name_len)?).into_owned();
            let marked = [uncomp_size, comp_size, local_offset].map(|v| v == ZIP64_SENTINEL);
            let [uncomp64, comp64, offset64] = if marked.contains(&true) {
                zip64_values(span(data, p + 46 + name_len, extra_len)?, marked)?
            } else {
                [None; 3]
            };
            entries.push(ZipEntry {
                name,
                method,
                comp_size: comp64.unwrap_or(comp_size as u64),
                uncomp_size: uncomp64.unwrap_or(uncomp_size as u64),
                local_offset: offset64.unwrap_or(local_offset as u64),
            });
            p += 46 + name_len + extra_len + comment_len;
        }
        Some(ZipArchive { data, entries })
    }

    pub fn find(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    pub fn extract(&self, entry: &ZipEntry) -> Option<Vec<u8>> {
        let p = usize::try_from(entry.local_offset).ok()?;
        let head = span(self.data, p, 30)?;
        if rd32(head) != LOCAL_SIG {
            return None;
        }
        let name_len = rd16(&head[26..]) as usize;
        let extra_len = rd16(&head[28..]) as usize;
        let data_offset = p + 30 + name_len + extra_len;
        let src = span(
            self.data,
            data_offset,
            usize::try_from(entry.comp_size).ok()?,
        )?;
        if entry.method == 0 {
            if entry.comp_size != entry.uncomp_size {
                return None;
            }
            return Some(src.to_vec());
        }
        if entry.method == 8 {
            // No stream at all for an empty entry: zero compressed bytes is
            // not a valid deflate stream, but writers emit it for empty
            // files and directory entries, and other readers (LibreOffice
            // among them) open such packages.
            if entry.comp_size == 0 && entry.uncomp_size == 0 {
                return Some(Vec::new());
            }
            let out = inflate_raw(src, deflate_cap(entry.uncomp_size)?)?;
            if out.len() as u64 == entry.uncomp_size {
                return Some(out);
            }
            return None;
        }
        None
    }

    /// Convenience: find by name and extract in one call.
    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        self.extract(self.find(name)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal valid ZIP with all entries using the STORED method.
    fn make_stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut locals: Vec<u32> = Vec::new();

        // Local file headers + data.
        for (name, data) in files {
            locals.push(out.len() as u32);
            let nb = name.as_bytes();
            out.extend_from_slice(&LOCAL_SIG.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method = STORED
            out.extend_from_slice(&0u16.to_le_bytes()); // mod time
            out.extend_from_slice(&0u16.to_le_bytes()); // mod date
            out.extend_from_slice(&0u32.to_le_bytes()); // crc32 (unused by reader)
            out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // comp size
            out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // uncomp size
            out.extend_from_slice(&(nb.len() as u16).to_le_bytes()); // name len
            out.extend_from_slice(&0u16.to_le_bytes()); // extra len
            out.extend_from_slice(nb);
            out.extend_from_slice(data);
        }

        // Central directory.
        let cd_offset = out.len() as u32;
        for (i, (name, data)) in files.iter().enumerate() {
            let nb = name.as_bytes();
            out.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version made by
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method
            out.extend_from_slice(&0u16.to_le_bytes()); // mod time
            out.extend_from_slice(&0u16.to_le_bytes()); // mod date
            out.extend_from_slice(&0u32.to_le_bytes()); // crc32
            out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // comp size
            out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // uncomp size
            out.extend_from_slice(&(nb.len() as u16).to_le_bytes()); // name len
            out.extend_from_slice(&0u16.to_le_bytes()); // extra len
            out.extend_from_slice(&0u16.to_le_bytes()); // comment len
            out.extend_from_slice(&0u16.to_le_bytes()); // disk start
            out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            out.extend_from_slice(&locals[i].to_le_bytes()); // local header offset
            out.extend_from_slice(nb);
        }
        let cd_size = out.len() as u32 - cd_offset;

        // End of central directory.
        out.extend_from_slice(&EOCD_SIG.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // disk number
        out.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
        out.extend_from_slice(&(files.len() as u16).to_le_bytes()); // entries on disk
        out.extend_from_slice(&(files.len() as u16).to_le_bytes()); // entries total
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }

    #[test]
    fn open_find_extract_single() {
        let zip = make_stored_zip(&[("hello.txt", b"world")]);
        let arc = ZipArchive::open(&zip).expect("open");
        assert_eq!(arc.entries().len(), 1);
        let e = arc.find("hello.txt").expect("find");
        assert_eq!(e.method, 0);
        assert_eq!(arc.extract(e).unwrap(), b"world");
        assert_eq!(arc.read("hello.txt").unwrap(), b"world");
    }

    #[test]
    fn multiple_entries_and_ordering() {
        let zip = make_stored_zip(&[
            ("[Content_Types].xml", b"<types/>"),
            ("word/document.xml", b"<document>hi</document>"),
        ]);
        let arc = ZipArchive::open(&zip).expect("open");
        assert_eq!(arc.entries().len(), 2);
        assert_eq!(
            arc.read("word/document.xml").unwrap(),
            b"<document>hi</document>"
        );
        assert_eq!(arc.read("[Content_Types].xml").unwrap(), b"<types/>");
    }

    #[test]
    fn missing_entry_returns_none() {
        let zip = make_stored_zip(&[("a", b"1")]);
        let arc = ZipArchive::open(&zip).expect("open");
        assert!(arc.find("does/not/exist").is_none());
        assert!(arc.read("nope").is_none());
    }

    #[test]
    fn too_small_is_rejected() {
        assert!(ZipArchive::open(&[0u8; 4]).is_none());
    }

    /// Wraps `stream` as the single entry "a" of a ZIP, switched to the deflate
    /// method and declaring `declared` uncompressed bytes in both headers.
    fn deflate_zip(stream: &[u8], declared: u32) -> Vec<u8> {
        let mut zip = make_stored_zip(&[("a", stream)]);
        let eocd = zip.len() - 22;
        let central = rd32(&zip[eocd + 16..]) as usize;
        for (method, uncomp) in [(8, 22), (central + 10, central + 24)] {
            zip[method..method + 2].copy_from_slice(&8u16.to_le_bytes());
            zip[uncomp..uncomp + 4].copy_from_slice(&declared.to_le_bytes());
        }
        zip
    }

    /// #451: a declared size of 0 must not become inflate's "unlimited" cap.
    #[test]
    fn deflate_cap_never_uncapped() {
        assert_eq!(deflate_cap(0), Some(1));
        assert_eq!(deflate_cap(1), Some(1));
        assert_eq!(deflate_cap(4096), Some(4096));
        assert_eq!(deflate_cap(u32::MAX as u64), Some(u32::MAX as usize));
    }

    /// Behaviour (also holds before #451): a zero-declared deflate entry whose
    /// stream yields data is rejected.
    #[test]
    fn zero_declared_deflate_entry_with_data_is_rejected() {
        // BFINAL=1, BTYPE=00 (stored block) carrying five bytes.
        let mut stream = vec![0x01, 5, 0, !5u8, 0xFF];
        stream.extend_from_slice(b"hello");
        let zip = deflate_zip(&stream, 0);
        let arc = ZipArchive::open(&zip).expect("open");
        assert!(arc.read("a").is_none());
        // The same stream declaring its real size extracts.
        let zip = deflate_zip(&stream, 5);
        assert_eq!(ZipArchive::open(&zip).unwrap().read("a").unwrap(), b"hello");
    }

    /// Behaviour (also holds before #451): a genuinely empty deflate entry
    /// still extracts as empty.
    #[test]
    fn zero_declared_empty_deflate_extracts_empty() {
        // Fixed-Huffman block holding only the end-of-block symbol.
        let zip = deflate_zip(&[0x03, 0x00], 0);
        let arc = ZipArchive::open(&zip).expect("open");
        assert_eq!(arc.read("a"), Some(Vec::new()));
    }

    /// #1064: an empty file or directory entry written as deflate with no
    /// stream bytes at all extracts as empty, as other readers allow.
    #[test]
    fn deflate_entry_with_no_stream_bytes_extracts_empty() {
        let zip = deflate_zip(&[], 0);
        let arc = ZipArchive::open(&zip).expect("open");
        assert_eq!(arc.read("a"), Some(Vec::new()));
    }

    /// One entry for [`zip64`]: name, method, the bytes stored in the
    /// archive and the uncompressed size they declare.
    struct Entry64<'a> {
        name: &'a str,
        method: u16,
        stored: Vec<u8>,
        uncomp: u64,
    }

    /// A ZIP the way ZIP64 writers lay it out. With `sizes64`, every central
    /// entry's sizes are [`ZIP64_SENTINEL`] and live in its 0x0001 extra field
    /// (the shape of LibreOffice's tdf82984_zip64XLSXImport.xlsx). With
    /// `end64`, the local header offsets are ZIP64 too, and the end record's
    /// count and offset are sentinels: the real ones sit in a ZIP64 end record
    /// that a locator before the end record points to.
    fn zip64(files: &[Entry64], sizes64: bool, end64: bool) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut locals: Vec<u64> = Vec::new();
        for f in files {
            locals.push(out.len() as u64);
            out.extend_from_slice(&LOCAL_SIG.to_le_bytes());
            out.extend_from_slice(&45u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&f.method.to_le_bytes());
            out.extend_from_slice(&[0; 8]); // time, date, crc32
            out.extend_from_slice(&ZIP64_SENTINEL.to_le_bytes());
            out.extend_from_slice(&ZIP64_SENTINEL.to_le_bytes());
            out.extend_from_slice(&(f.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // extra len
            out.extend_from_slice(f.name.as_bytes());
            out.extend_from_slice(&ZIP64_EXTRA_ID.to_le_bytes());
            out.extend_from_slice(&16u16.to_le_bytes());
            out.extend_from_slice(&f.uncomp.to_le_bytes());
            out.extend_from_slice(&(f.stored.len() as u64).to_le_bytes());
            out.extend_from_slice(&f.stored);
        }
        let cd_offset = out.len() as u64;
        for (f, local) in files.iter().zip(&locals) {
            let mut extra: Vec<u8> = Vec::new();
            let (uncomp32, comp32) = if sizes64 {
                extra.extend_from_slice(&f.uncomp.to_le_bytes());
                extra.extend_from_slice(&(f.stored.len() as u64).to_le_bytes());
                (ZIP64_SENTINEL, ZIP64_SENTINEL)
            } else {
                (f.uncomp as u32, f.stored.len() as u32)
            };
            let offset32 = if end64 {
                extra.extend_from_slice(&local.to_le_bytes());
                ZIP64_SENTINEL
            } else {
                *local as u32
            };
            // Another field before the ZIP64 one: the reader walks to it.
            let mut extras = vec![0x55, 0x54, 1, 0, 0];
            if !extra.is_empty() {
                extras.extend_from_slice(&ZIP64_EXTRA_ID.to_le_bytes());
                extras.extend_from_slice(&(extra.len() as u16).to_le_bytes());
                extras.extend_from_slice(&extra);
            }
            out.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
            out.extend_from_slice(&45u16.to_le_bytes()); // version made by
            out.extend_from_slice(&45u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&f.method.to_le_bytes());
            out.extend_from_slice(&[0; 8]); // time, date, crc32
            out.extend_from_slice(&comp32.to_le_bytes());
            out.extend_from_slice(&uncomp32.to_le_bytes());
            out.extend_from_slice(&(f.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&(extras.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // comment len
            out.extend_from_slice(&[0; 8]); // disk start, attrs
            out.extend_from_slice(&offset32.to_le_bytes());
            out.extend_from_slice(f.name.as_bytes());
            out.extend_from_slice(&extras);
        }
        let cd_size = out.len() as u64 - cd_offset;
        let count = files.len() as u64;
        if end64 {
            let record = out.len() as u64;
            out.extend_from_slice(&EOCD64_SIG.to_le_bytes());
            out.extend_from_slice(&44u64.to_le_bytes()); // record size
            out.extend_from_slice(&45u16.to_le_bytes()); // version made by
            out.extend_from_slice(&45u16.to_le_bytes()); // version needed
            out.extend_from_slice(&[0; 8]); // disk numbers
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&cd_size.to_le_bytes());
            out.extend_from_slice(&cd_offset.to_le_bytes());
            out.extend_from_slice(&EOCD64_LOCATOR_SIG.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes()); // disk
            out.extend_from_slice(&record.to_le_bytes());
            out.extend_from_slice(&1u32.to_le_bytes()); // total disks
        }
        let (count16, cd_size32, cd_offset32) = if end64 {
            (0xFFFF, ZIP64_SENTINEL, ZIP64_SENTINEL)
        } else {
            (count as u16, cd_size as u32, cd_offset as u32)
        };
        out.extend_from_slice(&EOCD_SIG.to_le_bytes());
        out.extend_from_slice(&[0; 4]); // disk numbers
        out.extend_from_slice(&count16.to_le_bytes());
        out.extend_from_slice(&count16.to_le_bytes());
        out.extend_from_slice(&cd_size32.to_le_bytes());
        out.extend_from_slice(&cd_offset32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }

    /// A stored entry and a deflate one (a stored deflate block) holding
    /// the same text.
    fn zip64_entries() -> Vec<Entry64<'static>> {
        let mut deflated = vec![0x01, 5, 0, !5u8, 0xFF];
        deflated.extend_from_slice(b"hello");
        vec![
            Entry64 {
                name: "stored.xml",
                method: 0,
                stored: b"hello".to_vec(),
                uncomp: 5,
            },
            Entry64 {
                name: "deflated.xml",
                method: 8,
                stored: deflated,
                uncomp: 5,
            },
        ]
    }

    fn reads_both(zip: &[u8]) {
        let arc = ZipArchive::open(zip).expect("open");
        assert_eq!(arc.entries().len(), 2);
        assert_eq!(arc.read("stored.xml").unwrap(), b"hello");
        assert_eq!(arc.read("deflated.xml").unwrap(), b"hello");
    }

    /// #1094: sizes in the ZIP64 extra field behind a plain end record, as
    /// LibreOffice's ZIP64 sample has them.
    #[test]
    fn zip64_sizes_behind_a_plain_end_record() {
        let zip = zip64(&zip64_entries(), true, false);
        reads_both(&zip);
        let arc = ZipArchive::open(&zip).unwrap();
        let e = arc.find("deflated.xml").unwrap();
        assert_eq!((e.comp_size, e.uncomp_size), (10, 5));
    }

    /// #1094: entry count, directory offset and local header offsets all
    /// ZIP64, found through the locator.
    #[test]
    fn zip64_end_record_through_the_locator() {
        reads_both(&zip64(&zip64_entries(), true, true));
        reads_both(&zip64(&zip64_entries(), false, true));
    }

    /// The offset of the locator's ZIP64 end record within `zip`.
    fn locator_target(zip: &[u8]) -> usize {
        let locator = zip.len() - 22 - 20;
        assert_eq!(rd32(&zip[locator..]), EOCD64_LOCATOR_SIG);
        locator + 8
    }

    #[test]
    fn zip64_locator_to_nowhere_is_rejected() {
        let good = zip64(&zip64_entries(), true, true);
        let at = locator_target(&good);
        // Past the end, at a huge offset, and at bytes that are no record.
        for target in [good.len() as u64, u64::MAX, 0] {
            let mut zip = good.clone();
            zip[at..at + 8].copy_from_slice(&target.to_le_bytes());
            assert!(ZipArchive::open(&zip).is_none(), "target {target}");
        }
    }

    #[test]
    fn zip64_end_record_with_impossible_values_is_rejected() {
        let good = zip64(&zip64_entries(), true, true);
        let record = rd64(&good[locator_target(&good)..]) as usize;
        // An entry count no data could hold, then a directory offset past
        // the end: neither may panic or reserve for the count.
        for (field, value) in [(32, u64::MAX), (48, u64::MAX), (48, good.len() as u64)] {
            let mut zip = good.clone();
            zip[record + field..record + field + 8].copy_from_slice(&value.to_le_bytes());
            assert!(ZipArchive::open(&zip).is_none(), "+{field} = {value}");
        }
    }

    /// An ordinary archive whose last entry comment is 20 bytes that look
    /// like a ZIP64 locator pointing at `target`.
    fn locator_lookalike(content: &[u8], target: u64) -> Vec<u8> {
        let mut zip = make_stored_zip(&[("a", content)]);
        let eocd = zip.len() - 22;
        let central = rd32(&zip[eocd + 16..]) as usize;
        zip[central + 32..central + 34].copy_from_slice(&20u16.to_le_bytes());
        let mut comment = EOCD64_LOCATOR_SIG.to_le_bytes().to_vec();
        comment.extend_from_slice(&0u32.to_le_bytes());
        comment.extend_from_slice(&target.to_le_bytes());
        comment.extend_from_slice(&1u32.to_le_bytes());
        zip.splice(eocd..eocd, comment);
        let cd_size = rd32(&zip[zip.len() - 22 + 12..]) + 20;
        let at = zip.len() - 22 + 12;
        zip[at..at + 4].copy_from_slice(&cd_size.to_le_bytes());
        zip
    }

    /// An ordinary archive opens as before when the 20 bytes before its end
    /// record happen to read as a ZIP64 locator: one pointing at no ZIP64
    /// end record, and one pointing at bytes that look like one.
    #[test]
    fn locator_lookalike_in_an_ordinary_archive_is_ignored() {
        let zip = locator_lookalike(b"hello", 0);
        let arc = ZipArchive::open(&zip).expect("open");
        assert_eq!(arc.read("a").unwrap(), b"hello");
        // The entry's data is a ZIP64 end record claiming 5 entries at 0.
        let mut record = EOCD64_SIG.to_le_bytes().to_vec();
        record.extend_from_slice(&[0; 28]);
        record.extend_from_slice(&5u64.to_le_bytes());
        record.extend_from_slice(&[0; 8]);
        record.extend_from_slice(&0u64.to_le_bytes());
        let data_at = 30 + 1; // local header + name "a"
        let zip = locator_lookalike(&record, data_at);
        let arc = ZipArchive::open(&zip).expect("open");
        assert_eq!(arc.entries().len(), 1);
        assert_eq!(arc.read("a").unwrap(), record);
    }

    /// A ZIP64 archive truncated anywhere opens to `None` or to entries
    /// that do not extract, and never panics.
    #[test]
    fn truncated_zip64_never_panics() {
        let zip = zip64(&zip64_entries(), true, true);
        for len in 0..zip.len() {
            if let Some(arc) = ZipArchive::open(&zip[..len]) {
                for e in arc.entries() {
                    let _ = arc.extract(e);
                }
            }
        }
    }

    /// A sentinel with no ZIP64 field (none at all, or after an extra block
    /// that runs past its end) keeps its 32-bit value, as other readers keep
    /// it: that entry declares 4 GiB and does not extract, the others do. A
    /// ZIP64 field too short for the marked values is corrupt.
    #[test]
    fn zip64_sentinel_without_its_value() {
        let good = zip64(&zip64_entries(), true, false);
        let central = rd32(&good[good.len() - 22 + 16..]) as usize;
        let extra = central + 46 + "stored.xml".len();
        // The ZIP64 field follows the 5-byte 0x5455 one.
        let zip64_field = extra + 5;
        let mut renamed = good.clone();
        renamed[zip64_field..zip64_field + 2].copy_from_slice(&0x9999u16.to_le_bytes());
        let mut overrun = good.clone();
        overrun[extra + 2..extra + 4].copy_from_slice(&200u16.to_le_bytes());
        for zip in [renamed, overrun] {
            let arc = ZipArchive::open(&zip).expect("open");
            let e = arc.find("stored.xml").unwrap();
            let sentinel = ZIP64_SENTINEL as u64;
            assert_eq!((e.comp_size, e.uncomp_size), (sentinel, sentinel));
            assert!(arc.read("stored.xml").is_none());
            assert_eq!(arc.read("deflated.xml").unwrap(), b"hello");
        }
        let mut short = good.clone();
        short[zip64_field + 2..zip64_field + 4].copy_from_slice(&8u16.to_le_bytes());
        assert!(ZipArchive::open(&short).is_none());
    }

    /// A ZIP64 entry declaring more bytes than the archive holds does not
    /// extract.
    #[test]
    fn zip64_sizes_past_the_data_do_not_extract() {
        for uncomp in [u64::MAX, 1 << 40] {
            let mut entries = zip64_entries();
            entries[1].uncomp = uncomp;
            let mut zip = zip64(&entries, true, false);
            let arc = ZipArchive::open(&zip).unwrap();
            assert!(arc.read("deflated.xml").is_none());
            // Compressed size past the end too.
            let central = rd32(&zip[zip.len() - 22 + 16..]) as usize;
            let second = central + 46 + "stored.xml".len() + 5 + 4 + 16;
            let comp = second + 46 + "deflated.xml".len() + 5 + 4 + 8;
            zip[comp..comp + 8].copy_from_slice(&u64::MAX.to_le_bytes());
            let arc = ZipArchive::open(&zip).unwrap();
            assert_eq!(arc.find("deflated.xml").unwrap().comp_size, u64::MAX);
            assert!(arc.read("deflated.xml").is_none());
            assert_eq!(arc.read("stored.xml").unwrap(), b"hello");
        }
    }

    #[test]
    fn garbage_without_eocd_rejected() {
        let junk = vec![0xABu8; 128];
        assert!(ZipArchive::open(&junk).is_none());
    }
}
