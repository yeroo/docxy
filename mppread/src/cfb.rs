//! Reader (and a minimal writer) for the OLE2 **Compound File Binary** format,
//! a.k.a. the Microsoft Compound Document / "structured storage" container.
//!
//! This is the outer wrapper of every legacy binary Office file — `.doc`,
//! `.xls`, and crucially `.mpp` — a little filesystem-in-a-file: a FAT-style
//! sector table, a directory of *storages* (folders) and *streams* (files), and
//! a "mini stream" for packing small streams tightly. Unlike the streams inside
//! a `.mpp` (which are undocumented), the container itself is a published spec
//! ([MS-CFB]), so this layer is exact and testable.
//!
//! Scope: reads v3 (512-byte sector) and v4 (4096-byte sector) files, following
//! the DIFAT/FAT/mini-FAT chains and the directory red-black tree. Streams are
//! exposed by leaf name ([`Cfb::read_stream`]) and by full path through the
//! storage hierarchy ([`Cfb::paths`] / [`Cfb::read_path`], e.g.
//! `"TBkndTask/FixedData"` — how `.mpp` nests its task/resource blocks). A small
//! v3 writer ([`write_cfb`] / [`write_cfb_tree`]) round-trip tests the reader,
//! nesting included, without shipping a binary fixture.
//!
//! [MS-CFB]: https://learn.microsoft.com/openspecs/windows_protocols/ms-cfb/

const SIG: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
const FREESECT: u32 = 0xFFFF_FFFF;
const FATSECT: u32 = 0xFFFF_FFFD;
const NOSTREAM: u32 = 0xFFFF_FFFF;

const DIR_ENTRY_SIZE: usize = 128;
const MINI_SECTOR_SIZE: usize = 64;

fn u16le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}
fn u32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
fn u64le(b: &[u8], off: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(v)
}

/// One directory entry: a storage (folder), stream (file), or the root.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// 1 = storage, 2 = stream, 5 = root storage.
    pub kind: u8,
    start: u32,
    size: u64,
    /// Red-black-tree links (entry indices, or `NOSTREAM`): the left/right
    /// siblings within the parent, and the child that roots this storage's
    /// contents.
    left: u32,
    right: u32,
    child: u32,
}

impl Entry {
    pub fn is_stream(&self) -> bool {
        self.kind == 2
    }
    pub fn is_storage(&self) -> bool {
        self.kind == 1 || self.kind == 5
    }
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// A parsed compound file.
pub struct Cfb {
    data: Vec<u8>,
    sector_size: usize,
    mini_cutoff: u64,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    mini_stream: Vec<u8>,
    entries: Vec<Entry>,
}

impl Cfb {
    /// Parse a compound file from its raw bytes.
    pub fn open(bytes: &[u8]) -> Result<Cfb, String> {
        if bytes.len() < 512 || bytes[..8] != SIG {
            return Err("not a compound file (bad signature)".into());
        }
        let major = u16le(bytes, 26);
        let sector_shift = u16le(bytes, 30);
        let sector_size = 1usize << sector_shift;
        if (major == 3 && sector_size != 512) || (major == 4 && sector_size != 4096) {
            return Err(format!("unexpected sector size {sector_size} for v{major}"));
        }
        let num_fat_sectors = u32le(bytes, 44);
        let first_dir = u32le(bytes, 48);
        let mini_cutoff = u32le(bytes, 56) as u64;
        let first_minifat = u32le(bytes, 60);
        let num_minifat = u32le(bytes, 64);
        let first_difat = u32le(bytes, 68);
        let num_difat = u32le(bytes, 72);

        let data = bytes.to_vec();
        let mut cfb = Cfb {
            data,
            sector_size,
            mini_cutoff,
            fat: Vec::new(),
            minifat: Vec::new(),
            mini_stream: Vec::new(),
            entries: Vec::new(),
        };

        // 1) DIFAT: 109 FAT-sector pointers in the header, then any DIFAT-sector chain.
        let mut difat: Vec<u32> = (0..109).map(|i| u32le(&cfb.data, 76 + i * 4)).collect();
        let mut sec = first_difat;
        let mut guard = 0;
        while sec != ENDOFCHAIN && sec != FREESECT && guard <= num_difat {
            let off = cfb.sector_offset(sec);
            let per = sector_size / 4 - 1; // last slot chains to the next DIFAT sector
            for i in 0..per {
                difat.push(cfb.read_u32(off + i * 4));
            }
            sec = cfb.read_u32(off + sector_size - 4);
            guard += 1;
        }

        // 2) FAT: concatenate the FAT sectors the DIFAT points at, stopping once
        //    the header's declared FAT-sector count is satisfied.
        let want_fat = num_fat_sectors as usize;
        let mut read_fat = 0usize;
        for &fs in difat.iter() {
            if want_fat > 0 && read_fat >= want_fat {
                break;
            }
            if fs == FREESECT || fs == ENDOFCHAIN {
                continue;
            }
            let off = cfb.sector_offset(fs);
            if off + sector_size > cfb.data.len() {
                continue;
            }
            for i in 0..sector_size / 4 {
                cfb.fat.push(cfb.read_u32(off + i * 4));
            }
            read_fat += 1;
        }

        // 3) Directory: follow the FAT chain from the first directory sector.
        //    Keep *every* 128-byte slot (including unused ones) so the entry
        //    indices stay aligned with the left/right/child pointers.
        let dir_bytes = cfb.read_fat_chain(first_dir, None);
        for chunk in dir_bytes.chunks(DIR_ENTRY_SIZE) {
            if chunk.len() < DIR_ENTRY_SIZE {
                break;
            }
            let kind = chunk[66];
            let name_len = u16le(chunk, 64) as usize;
            let name = if kind == 0 {
                String::new()
            } else {
                utf16_name(&chunk[0..64], name_len)
            };
            let start = u32le(chunk, 116);
            let mut size = u64le(chunk, 120);
            if sector_size == 512 {
                size &= 0xFFFF_FFFF; // v3: high 32 bits are reserved
            }
            cfb.entries.push(Entry {
                name,
                kind,
                start,
                size,
                left: u32le(chunk, 68),
                right: u32le(chunk, 72),
                child: u32le(chunk, 76),
            });
        }

        // 4) Root entry defines the mini stream; mini-FAT indexes it.
        if let Some(root) = cfb.entries.iter().find(|e| e.kind == 5).cloned() {
            cfb.mini_stream = cfb.read_fat_chain(root.start, Some(root.size as usize));
        }
        if first_minifat != ENDOFCHAIN && num_minifat > 0 {
            let mf = cfb.read_fat_chain(first_minifat, None);
            cfb.minifat = mf
                .chunks(4)
                .map(|c| {
                    let mut v = [0u8; 4];
                    v[..c.len()].copy_from_slice(c);
                    u32::from_le_bytes(v)
                })
                .collect();
        }

        Ok(cfb)
    }

    fn sector_offset(&self, sec: u32) -> usize {
        (sec as usize + 1) * self.sector_size
    }

    fn read_u32(&self, off: usize) -> u32 {
        if off + 4 <= self.data.len() {
            u32le(&self.data, off)
        } else {
            FREESECT
        }
    }

    /// Concatenate the sectors of a FAT chain starting at `start`, optionally
    /// truncated to `size` bytes. Guards against cycles via a step cap.
    fn read_fat_chain(&self, start: u32, size: Option<usize>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut sec = start;
        let mut steps = 0;
        let cap = self.fat.len() + 2;
        while sec != ENDOFCHAIN && sec != FREESECT && steps <= cap {
            let off = self.sector_offset(sec);
            if off + self.sector_size > self.data.len() {
                break;
            }
            out.extend_from_slice(&self.data[off..off + self.sector_size]);
            sec = self.fat.get(sec as usize).copied().unwrap_or(ENDOFCHAIN);
            steps += 1;
        }
        if let Some(n) = size {
            out.truncate(n);
        }
        out
    }

    /// Read a small stream out of the mini stream via the mini-FAT chain.
    fn read_mini_chain(&self, start: u32, size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut sec = start;
        let mut steps = 0;
        let cap = self.minifat.len() + 2;
        while sec != ENDOFCHAIN && sec != FREESECT && steps <= cap {
            let off = sec as usize * MINI_SECTOR_SIZE;
            if off + MINI_SECTOR_SIZE > self.mini_stream.len() {
                break;
            }
            out.extend_from_slice(&self.mini_stream[off..off + MINI_SECTOR_SIZE]);
            sec = self
                .minifat
                .get(sec as usize)
                .copied()
                .unwrap_or(ENDOFCHAIN);
            steps += 1;
        }
        out.truncate(size);
        out
    }

    /// All *used* directory entries (storages, streams, root), in file order.
    pub fn entries(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.kind != 0).collect()
    }

    /// Names of every stream in the file (flat; storage nesting is ignored).
    /// Prefer [`paths`](Cfb::paths) to disambiguate same-named streams in
    /// different storages.
    pub fn stream_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| e.is_stream())
            .map(|e| e.name.clone())
            .collect()
    }

    /// Read a stream's bytes by leaf name (first match). Chooses the mini stream
    /// or the regular FAT chain based on the stream's size.
    pub fn read_stream(&self, name: &str) -> Option<Vec<u8>> {
        let e = self
            .entries
            .iter()
            .find(|e| e.is_stream() && e.name == name)?;
        Some(self.read_entry(e))
    }

    /// The size-routed bytes of a directory entry.
    fn read_entry(&self, e: &Entry) -> Vec<u8> {
        if e.size < self.mini_cutoff {
            self.read_mini_chain(e.start, e.size as usize)
        } else {
            self.read_fat_chain(e.start, Some(e.size as usize))
        }
    }

    /// Index of the root storage entry (type 5).
    fn root_index(&self) -> Option<usize> {
        self.entries.iter().position(|e| e.kind == 5)
    }

    /// The direct children of a storage, in the directory's in-order sequence.
    /// (The CFB directory is a red-black tree; an in-order walk of a storage's
    /// child subtree yields its entries.)
    fn children_of(&self, storage_idx: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack: Vec<u32> = Vec::new();
        let mut cur = self
            .entries
            .get(storage_idx)
            .map(|e| e.child)
            .unwrap_or(NOSTREAM);
        let cap = self.entries.len() + 1;
        while (!stack.is_empty() || cur != NOSTREAM) && out.len() <= cap {
            while cur != NOSTREAM && (cur as usize) < self.entries.len() {
                stack.push(cur);
                cur = self.entries[cur as usize].left;
            }
            let Some(n) = stack.pop() else { break };
            out.push(n as usize);
            cur = self.entries[n as usize].right;
        }
        out
    }

    /// Full stream paths (`storage/sub/stream`) for every stream, walking the
    /// storage tree from the root.
    pub fn paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(root) = self.root_index() {
            self.collect_paths(root, "", &mut out);
        }
        out
    }

    fn collect_paths(&self, storage_idx: usize, prefix: &str, out: &mut Vec<String>) {
        for c in self.children_of(storage_idx) {
            let e = &self.entries[c];
            let path = if prefix.is_empty() {
                e.name.clone()
            } else {
                format!("{prefix}/{}", e.name)
            };
            if e.is_stream() {
                out.push(path);
            } else if e.is_storage() {
                self.collect_paths(c, &path, out);
            }
        }
    }

    /// Read a stream by its full path, e.g. `"TBkndTask/FixedData"`. Navigates
    /// the storage tree; returns `None` if the path doesn't resolve to a stream.
    pub fn read_path(&self, path: &str) -> Option<Vec<u8>> {
        let mut idx = self.root_index()?;
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            let child = self
                .children_of(idx)
                .into_iter()
                .find(|&c| self.entries[c].name == *part)?;
            let e = &self.entries[child];
            if i == parts.len() - 1 {
                return e.is_stream().then(|| self.read_entry(e));
            }
            if !e.is_storage() {
                return None;
            }
            idx = child;
        }
        None
    }
}

/// Decode a UTF-16LE directory name; `name_len` is the byte length including the
/// trailing NUL, as stored in the entry.
fn utf16_name(raw: &[u8], name_len: usize) -> String {
    let chars = name_len.saturating_sub(2) / 2; // drop the NUL terminator
    let units: Vec<u16> = (0..chars).map(|i| u16le(raw, i * 2)).collect();
    String::from_utf16_lossy(&units)
}

// ---- minimal v3 writer (test support) ---------------------------------------

/// A node in a compound file's directory tree, for the test writer.
pub enum Node<'a> {
    Stream(&'a str, Vec<u8>),
    Storage(&'a str, Vec<Node<'a>>),
}

/// One directory entry under construction.
struct DirEnt {
    name: String,
    kind: u8,
    data: Option<Vec<u8>>, // stream payload
    child: u32,
    right: u32,
}

/// Assign directory indices to a sibling group, chaining them via right-siblings
/// (a valid, if unbalanced, red-black tree). Returns the first child's index.
fn assign_dir(nodes: &[Node], dir: &mut Vec<DirEnt>) -> u32 {
    if nodes.is_empty() {
        return NOSTREAM;
    }
    let base = dir.len() as u32;
    for _ in nodes {
        dir.push(DirEnt {
            name: String::new(),
            kind: 0,
            data: None,
            child: NOSTREAM,
            right: NOSTREAM,
        });
    }
    for (i, node) in nodes.iter().enumerate() {
        let idx = base as usize + i;
        let right = if i + 1 < nodes.len() {
            base + i as u32 + 1
        } else {
            NOSTREAM
        };
        dir[idx] = match node {
            Node::Stream(name, data) => DirEnt {
                name: (*name).to_string(),
                kind: 2,
                data: Some(data.clone()),
                child: NOSTREAM,
                right,
            },
            Node::Storage(name, kids) => {
                let child = assign_dir(kids, dir);
                DirEnt {
                    name: (*name).to_string(),
                    kind: 1,
                    data: None,
                    child,
                    right,
                }
            }
        };
    }
    base
}

/// Assemble a minimal v3 compound file with `streams` (name, bytes) directly
/// under the root storage. See [`write_cfb_tree`] for nested storages.
pub fn write_cfb(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let nodes: Vec<Node> = streams
        .iter()
        .map(|(n, d)| Node::Stream(n, d.clone()))
        .collect();
    write_cfb_tree(&nodes)
}

/// Assemble a minimal v3 compound file from a directory tree (streams and
/// nested storages). Small streams go in the mini stream, large ones in the FAT.
/// Intentionally minimal — an unbalanced (right-leaning) directory tree, enough
/// to exercise the reader's navigation in tests, not a general CFB authoring
/// tool.
pub fn write_cfb_tree(root_children: &[Node]) -> Vec<u8> {
    const SS: usize = 512;
    const CUTOFF: usize = 4096;

    // 1) Build the directory tree (index 0 = root storage).
    let mut dir: Vec<DirEnt> = vec![DirEnt {
        name: "Root Entry".into(),
        kind: 5,
        data: None,
        child: NOSTREAM,
        right: NOSTREAM,
    }];
    let rc = assign_dir(root_children, &mut dir);
    dir[0].child = rc;

    // Sectors accumulate here; `fat[i]` is the next-pointer for sector i.
    let mut sectors: Vec<[u8; SS]> = Vec::new();
    let mut fat: Vec<u32> = Vec::new();
    let alloc = |bytes: &[u8], sectors: &mut Vec<[u8; SS]>, fat: &mut Vec<u32>| -> u32 {
        if bytes.is_empty() {
            return ENDOFCHAIN;
        }
        let start = sectors.len() as u32;
        let n = bytes.len().div_ceil(SS);
        for i in 0..n {
            let mut s = [0u8; SS];
            let a = i * SS;
            let b = (a + SS).min(bytes.len());
            s[..b - a].copy_from_slice(&bytes[a..b]);
            sectors.push(s);
            let idx = start as usize + i;
            fat.push(if i + 1 < n {
                (idx + 1) as u32
            } else {
                ENDOFCHAIN
            });
        }
        start
    };

    // 2) Place each stream: small → mini stream, large → FAT. `place[i]` is the
    //    (start, size, is_mini) for directory entry i.
    let mut mini_stream: Vec<u8> = Vec::new();
    let mut minifat: Vec<u32> = Vec::new();
    let mut place: Vec<(u32, u64, bool)> = vec![(ENDOFCHAIN, 0, false); dir.len()];
    for (idx, ent) in dir.iter().enumerate() {
        if ent.kind != 2 {
            continue;
        }
        let data = ent.data.as_ref().unwrap();
        if data.is_empty() {
            continue;
        } else if data.len() < CUTOFF {
            let start = (mini_stream.len() / MINI_SECTOR_SIZE) as u32;
            let n = data.len().div_ceil(MINI_SECTOR_SIZE);
            for i in 0..n {
                let a = i * MINI_SECTOR_SIZE;
                let b = (a + MINI_SECTOR_SIZE).min(data.len());
                mini_stream.extend_from_slice(&data[a..b]);
                mini_stream.resize(mini_stream.len() + (MINI_SECTOR_SIZE - (b - a)), 0);
                let mi = start as usize + i;
                minifat.push(if i + 1 < n {
                    (mi + 1) as u32
                } else {
                    ENDOFCHAIN
                });
            }
            place[idx] = (start, data.len() as u64, true);
        } else {
            place[idx] = (u32::MAX, data.len() as u64, false);
        }
    }

    // Allocate big streams in the FAT.
    for idx in 0..dir.len() {
        if dir[idx].kind == 2 && !place[idx].2 && place[idx].1 as usize >= CUTOFF {
            let data = dir[idx].data.clone().unwrap();
            place[idx].0 = alloc(&data, &mut sectors, &mut fat);
        }
    }

    // 3) Mini stream + mini-FAT as regular chains.
    let mini_start = alloc(&mini_stream, &mut sectors, &mut fat);
    let minifat_bytes: Vec<u8> = minifat.iter().flat_map(|v| v.to_le_bytes()).collect();
    let minifat_start = alloc(&minifat_bytes, &mut sectors, &mut fat);
    let num_minifat = if minifat_bytes.is_empty() {
        0
    } else {
        minifat_bytes.len().div_ceil(SS)
    };

    // 4) Serialize the directory entries with their tree pointers.
    let mut dbytes: Vec<u8> = Vec::new();
    for (idx, ent) in dir.iter().enumerate() {
        let (start, size) = match ent.kind {
            5 => (mini_start, mini_stream.len() as u64),
            2 => {
                let (s, sz, _) = place[idx];
                (if sz == 0 { ENDOFCHAIN } else { s }, sz)
            }
            _ => (ENDOFCHAIN, 0), // storage
        };
        let mut e = [0u8; DIR_ENTRY_SIZE];
        let utf16: Vec<u16> = ent.name.encode_utf16().collect();
        for (i, u) in utf16.iter().enumerate().take(31) {
            e[i * 2..i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        let name_len = if ent.kind == 0 {
            0
        } else {
            ((utf16.len().min(31) + 1) * 2) as u16
        };
        e[64..66].copy_from_slice(&name_len.to_le_bytes());
        e[66] = ent.kind;
        e[67] = 1; // color: black
        e[68..72].copy_from_slice(&NOSTREAM.to_le_bytes()); // left
        e[72..76].copy_from_slice(&ent.right.to_le_bytes());
        e[76..80].copy_from_slice(&ent.child.to_le_bytes());
        e[116..120].copy_from_slice(&start.to_le_bytes());
        e[120..128].copy_from_slice(&size.to_le_bytes());
        dbytes.extend_from_slice(&e);
    }
    while !dbytes.len().is_multiple_of(SS) {
        dbytes.extend_from_slice(&[0u8; DIR_ENTRY_SIZE]); // unused slots (kind 0)
    }
    let dir_start = alloc(&dbytes, &mut sectors, &mut fat);

    // 6) Reserve FAT sectors: solve for a self-consistent count.
    let m = sectors.len();
    let mut f = 1usize;
    loop {
        let total = m + f;
        let need = total.div_ceil(SS / 4);
        if need <= f {
            break;
        }
        f = need;
    }
    let fat_start = sectors.len();
    for _ in 0..f {
        sectors.push([0u8; SS]);
        fat.push(FATSECT);
    }

    // 7) Serialize the FAT into its sectors.
    let entries_per = SS / 4;
    for s in 0..f {
        let mut sect = [0u8; SS];
        for i in 0..entries_per {
            let gi = s * entries_per + i;
            let v = fat.get(gi).copied().unwrap_or(FREESECT);
            sect[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        sectors[fat_start + s] = sect;
    }

    // 8) Header.
    let mut header = [0u8; SS];
    header[..8].copy_from_slice(&SIG);
    header[24..26].copy_from_slice(&0x003Eu16.to_le_bytes()); // minor version
    header[26..28].copy_from_slice(&3u16.to_le_bytes()); // major v3
    header[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes()); // byte order
    header[30..32].copy_from_slice(&9u16.to_le_bytes()); // sector shift (512)
    header[32..34].copy_from_slice(&6u16.to_le_bytes()); // mini sector shift (64)
    header[44..48].copy_from_slice(&(f as u32).to_le_bytes()); // # FAT sectors
    header[48..52].copy_from_slice(&(dir_start).to_le_bytes()); // first dir sector
    header[56..60].copy_from_slice(&(CUTOFF as u32).to_le_bytes()); // mini cutoff
    header[60..64].copy_from_slice(&minifat_start.to_le_bytes()); // first mini-FAT
    header[64..68].copy_from_slice(&(num_minifat as u32).to_le_bytes()); // # mini-FAT
    header[68..72].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // first DIFAT
    header[72..76].copy_from_slice(&0u32.to_le_bytes()); // # DIFAT sectors
    // DIFAT array: the FAT sector indices, then FREESECT.
    for i in 0..109 {
        let v = if i < f {
            (fat_start + i) as u32
        } else {
            FREESECT
        };
        header[76 + i * 4..80 + i * 4].copy_from_slice(&v.to_le_bytes());
    }

    let mut out = Vec::with_capacity(SS * (sectors.len() + 1));
    out.extend_from_slice(&header);
    for s in &sectors {
        out.extend_from_slice(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_check() {
        assert!(Cfb::open(b"not a compound file at all, definitely not 512 bytes").is_err());
    }

    #[test]
    fn navigates_nested_storage_tree() {
        // A tree mirroring .mpp's shape: streams at the root plus a storage
        // ("TBkndTask") holding its own streams.
        let tree = vec![
            Node::Stream("\u{5}SummaryInformation", b"meta".to_vec()),
            Node::Storage(
                "TBkndTask",
                vec![
                    Node::Stream("FixedMeta", vec![1u8; 40]),
                    Node::Stream("FixedData", vec![2u8; 6000]), // big → FAT
                    Node::Stream("Var2Data", b"vars".repeat(10)),
                ],
            ),
            Node::Storage("TBkndRsc", vec![Node::Stream("FixedData", vec![9u8; 24])]),
        ];
        let bytes = write_cfb_tree(&tree);
        let cfb = Cfb::open(&bytes).unwrap();

        let mut paths = cfb.paths();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "\u{5}SummaryInformation".to_string(), // 0x05 sorts before letters
                "TBkndRsc/FixedData".to_string(),
                "TBkndTask/FixedData".to_string(),
                "TBkndTask/FixedMeta".to_string(),
                "TBkndTask/Var2Data".to_string(),
            ]
        );
        // path lookup resolves through storages, disambiguating same leaf names
        assert_eq!(
            cfb.read_path("TBkndTask/FixedData").unwrap(),
            vec![2u8; 6000]
        );
        assert_eq!(cfb.read_path("TBkndRsc/FixedData").unwrap(), vec![9u8; 24]);
        assert_eq!(
            cfb.read_path("TBkndTask/Var2Data").unwrap(),
            b"vars".repeat(10)
        );
        assert!(cfb.read_path("TBkndTask/Missing").is_none());
        assert!(cfb.read_path("TBkndTask").is_none()); // a storage, not a stream
    }

    #[test]
    fn round_trip_mini_and_big_streams() {
        // one small stream (mini-FAT path) and one large (regular FAT path)
        let small = b"hello project world".to_vec();
        let big = vec![0xABu8; 5000]; // > 4096 cutoff
        let bytes = write_cfb(&[("Summary", small.clone()), ("Payload", big.clone())]);

        assert_eq!(&bytes[..8], &SIG);
        let cfb = Cfb::open(&bytes).unwrap();

        let mut names = cfb.stream_names();
        names.sort();
        assert_eq!(names, vec!["Payload".to_string(), "Summary".to_string()]);

        assert_eq!(cfb.read_stream("Summary").unwrap(), small);
        assert_eq!(cfb.read_stream("Payload").unwrap(), big);
        assert!(cfb.read_stream("Missing").is_none());
    }

    #[test]
    fn round_trip_many_small_streams() {
        // several mini streams forces multi-entry mini-FAT chains
        let streams: Vec<(&str, Vec<u8>)> = vec![
            ("Props", b"AB".repeat(50)),   // 100 bytes -> 2 mini sectors
            ("Tasks", b"task".repeat(20)), // 80 bytes
            ("Empty", Vec::new()),         // zero-length
            ("Calendars", vec![7u8; 200]), // 200 bytes -> 4 mini sectors
        ];
        let bytes = write_cfb(&streams);
        let cfb = Cfb::open(&bytes).unwrap();
        for (name, data) in &streams {
            assert_eq!(
                cfb.read_stream(name).unwrap(),
                *data,
                "stream {name} mismatch"
            );
        }
        assert_eq!(cfb.entries().iter().filter(|e| e.is_stream()).count(), 4);
    }

    #[test]
    fn round_trip_multi_sector_big_stream() {
        // a stream spanning many FAT sectors, plus enough others to push the
        // FAT itself past one sector
        let big = (0..20000u32).map(|i| i as u8).collect::<Vec<_>>();
        let bytes = write_cfb(&[("Big", big.clone())]);
        let cfb = Cfb::open(&bytes).unwrap();
        assert_eq!(cfb.read_stream("Big").unwrap(), big);
    }
}
