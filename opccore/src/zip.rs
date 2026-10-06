//! Read-only ZIP reader (stored + deflate).
//!
//! Ported from rust365 (`src/zip.rs`), unchanged logic, plus unit tests.

use crate::inflate::inflate_raw;

pub struct ZipEntry {
    pub name: String,
    pub method: u16,
    pub comp_size: u32,
    pub uncomp_size: u32,
    pub local_offset: u32,
}

const EOCD_SIG: u32 = 0x06054b50;
const CENTRAL_SIG: u32 = 0x02014b50;
const LOCAL_SIG: u32 = 0x04034b50;

fn rd16(p: &[u8]) -> u16 {
    p[0] as u16 | ((p[1] as u16) << 8)
}
fn rd32(p: &[u8]) -> u32 {
    p[0] as u32 | ((p[1] as u32) << 8) | ((p[2] as u32) << 16) | ((p[3] as u32) << 24)
}

/// Output cap for inflating a deflate entry that declares `uncomp_size` bytes.
/// `inflate_raw` treats a cap of 0 as unlimited, so an entry declaring 0 bytes
/// gets a cap of 1: its stream is cut off within one block step instead of
/// inflating without bound, and the size check in `extract` still rejects any
/// non-empty output.
fn deflate_cap(uncomp_size: u32) -> usize {
    (uncomp_size as usize).max(1)
}

pub struct ZipArchive<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntry>,
}

impl<'a> ZipArchive<'a> {
    pub fn open(data: &'a [u8]) -> Option<ZipArchive<'a>> {
        let size = data.len();
        let mut entries = Vec::new();
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
        let count = rd16(&data[off + 10..]) as usize;
        let cd_offset = rd32(&data[off + 16..]) as usize;
        let mut p = cd_offset;
        entries.reserve(count);
        for _ in 0..count {
            if p + 46 > size || rd32(&data[p..]) != CENTRAL_SIG {
                return None;
            }
            let method = rd16(&data[p + 10..]);
            let comp_size = rd32(&data[p + 20..]);
            let uncomp_size = rd32(&data[p + 24..]);
            let name_len = rd16(&data[p + 28..]) as usize;
            let extra_len = rd16(&data[p + 30..]) as usize;
            let comment_len = rd16(&data[p + 32..]) as usize;
            let local_offset = rd32(&data[p + 42..]);
            if p + 46 + name_len > size {
                return None;
            }
            let name = String::from_utf8_lossy(&data[p + 46..p + 46 + name_len]).into_owned();
            entries.push(ZipEntry {
                name,
                method,
                comp_size,
                uncomp_size,
                local_offset,
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
        let p = entry.local_offset as usize;
        if p + 30 > self.data.len() || rd32(&self.data[p..]) != LOCAL_SIG {
            return None;
        }
        let name_len = rd16(&self.data[p + 26..]) as usize;
        let extra_len = rd16(&self.data[p + 28..]) as usize;
        let data_offset = p + 30 + name_len + extra_len;
        if data_offset + entry.comp_size as usize > self.data.len() {
            return None;
        }
        let src = &self.data[data_offset..data_offset + entry.comp_size as usize];
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
            let out = inflate_raw(src, deflate_cap(entry.uncomp_size))?;
            if out.len() == entry.uncomp_size as usize {
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
        assert_eq!(deflate_cap(0), 1);
        assert_eq!(deflate_cap(1), 1);
        assert_eq!(deflate_cap(4096), 4096);
        assert_eq!(deflate_cap(u32::MAX), u32::MAX as usize);
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

    #[test]
    fn garbage_without_eocd_rejected() {
        let junk = vec![0xABu8; 128];
        assert!(ZipArchive::open(&junk).is_none());
    }
}
