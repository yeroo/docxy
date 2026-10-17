//! System fonts for PDF export: the [`PdfFontProvider`] that terminal `docxy`
//! and the suite give `docxcore::export::to_pdf`.
//!
//! `docxcore` embeds and subsets the font bytes it is handed; this crate finds
//! them. [`FontSet`] indexes the font files of a few directories by reading
//! only each face's table directory, `name` and `head` (never a whole file),
//! picks a face by family and style, and keeps a list of script fallbacks
//! (Arial Unicode, Noto, DejaVu, PingFang, Malgun, ...) for characters the
//! run's own face lacks.
//!
//! Only TrueType-outline (`glyf`) faces are offered: `docxcore`'s subsetter
//! reads `glyf`/`loca`, so a CFF face counts as no face. A face whose OS/2
//! `fsType` forbids embedding is left out too. Either way the characters it
//! would have shown land in the export's report.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use docxcore::pdf_font::{PdfFace, PdfFontProvider};

/// Larger tables are not read (a corrupt directory must not allocate gigabytes).
const MAX_TABLE: u32 = 128 << 20;
const MAX_DEPTH: usize = 6;

/// Where fonts live on this OS.
fn system_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.extend(
            [
                "/System/Library/Fonts",
                "/System/Library/Fonts/Supplemental",
                "/Library/Fonts",
            ]
            .map(PathBuf::from),
        );
        dirs.extend(home.map(|h| h.join("Library/Fonts")));
    } else if cfg!(windows) {
        let windir =
            std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        dirs.push(windir.join("Fonts"));
        dirs.extend(
            std::env::var_os("LOCALAPPDATA")
                .map(|d| PathBuf::from(d).join(r"Microsoft\Windows\Fonts")),
        );
    } else {
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
        if let Some(h) = home {
            dirs.push(h.join(".fonts"));
            dirs.push(h.join(".local/share/fonts"));
        }
    }
    dirs
}

/// Families tried, in order, for a character the run's own face lacks.
const FALLBACK_FAMILIES: &[&str] = &[
    "Arial Unicode MS",
    "Noto Sans",
    "Noto Sans Arabic",
    "Noto Sans Hebrew",
    "Noto Sans CJK SC",
    "Noto Sans CJK JP",
    "Noto Sans SC",
    "DejaVu Sans",
    "Segoe UI",
    "Segoe UI Historic",
    "Segoe UI Symbol",
    "Microsoft YaHei",
    "Microsoft JhengHei",
    "MS Gothic",
    "Malgun Gothic",
    "Yu Gothic",
    "SimSun",
    "PingFang SC",
    "Hiragino Sans GB",
    "Hiragino Sans",
    "Apple SD Gothic Neo",
    "Geeza Pro",
    "Arial Hebrew",
    "Lucida Grande",
    "Apple Symbols",
    "STHeiti",
    "Menlo",
    "FreeSans",
];

/// Families tried for a run with no (or an unknown) font family.
const DEFAULT_FAMILIES: &[&str] = &[
    "Arial",
    "Helvetica",
    "Helvetica Neue",
    "Liberation Sans",
    "DejaVu Sans",
    "Noto Sans",
    "Segoe UI",
    "Roboto",
    "FreeSans",
];

/// Metric-compatible or look-alike families for the usual Office fonts.
fn aliases(family: &str) -> &'static [&'static str] {
    match family {
        "arial" | "arial narrow" => &["Liberation Sans", "Arimo", "Helvetica"],
        "calibri" | "calibri light" => &["Carlito", "Liberation Sans"],
        "cambria" => &["Caladea", "Liberation Serif"],
        "times new roman" | "times" => &["Liberation Serif", "Tinos", "Times", "DejaVu Serif"],
        "courier new" | "courier" | "consolas" => {
            &["Liberation Mono", "Cousine", "Menlo", "DejaVu Sans Mono"]
        }
        "georgia" | "garamond" | "book antiqua" => &["DejaVu Serif", "Liberation Serif"],
        "verdana" | "tahoma" | "trebuchet ms" | "segoe ui" => &["DejaVu Sans", "Liberation Sans"],
        _ => &[],
    }
}

// ---- reading font files ----

#[derive(Clone, Copy)]
struct TableRef {
    tag: [u8; 4],
    offset: u32,
    len: u32,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn read_at(f: &mut File, at: u64, len: usize) -> Option<Vec<u8>> {
    f.seek(SeekFrom::Start(at)).ok()?;
    let mut buf = vec![0; len];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// The number of faces in `f` (1 for a plain sfnt) and the sfnt header offset
/// of face `index`.
fn face_offsets(f: &mut File) -> Option<Vec<u64>> {
    let head = read_at(f, 0, 12)?;
    if &head[..4] == b"ttcf" {
        let n = u32_at(&head, 8)?.min(256) as usize;
        let offs = read_at(f, 12, 4 * n)?;
        (0..n).map(|i| Some(u32_at(&offs, 4 * i)? as u64)).collect()
    } else {
        Some(vec![0])
    }
}

/// The table directory of the sfnt at `base`; `None` for anything but a
/// TrueType-flavoured sfnt.
fn read_directory(f: &mut File, base: u64) -> Option<Vec<TableRef>> {
    let head = read_at(f, base, 12)?;
    let version = u32_at(&head, 0)?;
    if version != 0x0001_0000 && &head[..4] != b"true" {
        return None;
    }
    let n = u16_at(&head, 4)? as usize;
    let dir = read_at(f, base + 12, 16 * n)?;
    (0..n)
        .map(|i| {
            Some(TableRef {
                tag: dir[16 * i..16 * i + 4].try_into().ok()?,
                offset: u32_at(&dir, 16 * i + 8)?,
                len: u32_at(&dir, 16 * i + 12)?,
            })
        })
        .collect()
}

fn find_table(dir: &[TableRef], tag: &[u8; 4]) -> Option<TableRef> {
    dir.iter().find(|t| &t.tag == tag).copied()
}

fn read_table(f: &mut File, t: TableRef) -> Option<Vec<u8>> {
    if t.len > MAX_TABLE {
        return None;
    }
    read_at(f, t.offset as u64, t.len as usize)
}

/// A standalone sfnt holding just `tables`, enough for `ttf-parser` to read
/// metrics, the cmap and names from.
fn mini_sfnt(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let n = tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    let pos = 12 + 16 * n;
    let mut body = Vec::new();
    for (tag, data) in tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&((pos + body.len()) as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
    }
    out.extend(body);
    out
}

/// A font file's face in the index.
struct Entry {
    path: PathBuf,
    index: u32,
    family: String,
    bold: bool,
    italic: bool,
}

/// The family and style of face `idx` of `path`, `None` when it is not a
/// `glyf` face that may be embedded.
fn index_face(f: &mut File, path: &Path, idx: u32, base: u64) -> Option<Entry> {
    let dir = read_directory(f, base)?;
    find_table(&dir, b"glyf")?;
    find_table(&dir, b"loca")?;
    if let Some(os2) = find_table(&dir, b"OS/2") {
        let os2 = read_at(f, os2.offset as u64, (os2.len as usize).min(10))?;
        if u16_at(&os2, 8)? & 0x0002 != 0 {
            return None; // restricted licence embedding
        }
    }
    let name = read_table(f, find_table(&dir, b"name")?)?;
    let head = read_table(f, find_table(&dir, b"head")?)?;
    let mac_style = u16_at(&head, 44)?;
    let names = ttf_parser::name::Table::parse(&name)?;
    let pick = |id: u16| {
        names
            .names
            .into_iter()
            .filter(|n| n.name_id == id && n.is_unicode())
            .find_map(|n| n.to_string())
    };
    let family = pick(16).or_else(|| pick(1))?;
    Some(Entry {
        path: path.to_path_buf(),
        index: idx,
        family,
        bold: mac_style & 1 != 0,
        italic: mac_style & 2 != 0,
    })
}

fn scan(dir: &Path, depth: usize, out: &mut Vec<Entry>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            if depth < MAX_DEPTH {
                scan(&path, depth + 1, out);
            }
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if !matches!(ext.as_deref(), Some("ttf" | "ttc" | "otf" | "otc")) {
            continue;
        }
        let Ok(mut f) = File::open(&path) else {
            continue;
        };
        let Some(bases) = face_offsets(&mut f) else {
            continue;
        };
        for (i, base) in bases.into_iter().enumerate() {
            if let Some(entry) = index_face(&mut f, &path, i as u32, base) {
                out.push(entry);
            }
        }
    }
}

// ---- a face ----

/// One face of a font file, as `docxcore` needs it. The metrics are read
/// when the face is first picked; the whole file only when it is embedded.
pub struct SysFace {
    id: u64,
    path: PathBuf,
    index: u32,
    ps_name: String,
    units_per_em: u16,
    ascent: i16,
    descent: i16,
    cmap: HashMap<u32, u16>,
    advances: Vec<u16>,
    data: OnceCell<Rc<[u8]>>,
}

impl std::fmt::Debug for SysFace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SysFace")
            .field("ps_name", &self.ps_name)
            .field("path", &self.path)
            .field("index", &self.index)
            .finish()
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01B3);
    }
    h
}

impl SysFace {
    fn load(path: &Path, index: u32) -> Option<SysFace> {
        let mut f = File::open(path).ok()?;
        let base = *face_offsets(&mut f)?.get(index as usize)?;
        let dir = read_directory(&mut f, base)?;
        let mut tables = Vec::new();
        for tag in [
            b"head", b"hhea", b"maxp", b"hmtx", b"cmap", b"OS/2", b"name",
        ] {
            if let Some(t) = find_table(&dir, tag) {
                tables.push((*tag, read_table(&mut f, t)?));
            }
        }
        let mini = mini_sfnt(&tables);
        let face = ttf_parser::Face::parse(&mini, 0).ok()?;
        let mut cmap = HashMap::new();
        for sub in face.tables().cmap?.subtables {
            if sub.is_unicode() {
                sub.codepoints(|cp| {
                    if let Some(g) = sub.glyph_index(cp) {
                        cmap.entry(cp).or_insert(g.0);
                    }
                });
            }
        }
        let advances = (0..face.number_of_glyphs())
            .map(|g| face.glyph_hor_advance(ttf_parser::GlyphId(g)).unwrap_or(0))
            .collect();
        let ps_name = face
            .names()
            .into_iter()
            .filter(|n| n.name_id == 6 && n.is_unicode())
            .find_map(|n| n.to_string())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        let mut key = path.to_string_lossy().into_owned().into_bytes();
        key.extend(index.to_be_bytes());
        Some(SysFace {
            id: fnv(&key),
            path: path.to_path_buf(),
            index,
            ps_name,
            units_per_em: face.units_per_em(),
            ascent: face.ascender(),
            descent: face.descender(),
            cmap,
            advances,
            data: OnceCell::new(),
        })
    }
}

impl PdfFace for SysFace {
    fn id(&self) -> u64 {
        self.id
    }
    fn ps_name(&self) -> String {
        self.ps_name.clone()
    }
    fn units_per_em(&self) -> u16 {
        self.units_per_em
    }
    fn glyph(&self, ch: char) -> Option<u16> {
        self.cmap.get(&(ch as u32)).copied().filter(|&g| g != 0)
    }
    fn advance(&self, gid: u16) -> u16 {
        self.advances.get(gid as usize).copied().unwrap_or(0)
    }
    fn ascent(&self) -> i16 {
        self.ascent
    }
    fn descent(&self) -> i16 {
        self.descent
    }
    fn sfnt(&self) -> Rc<[u8]> {
        self.data
            .get_or_init(|| std::fs::read(&self.path).unwrap_or_default().into())
            .clone()
    }
    fn face_index(&self) -> u32 {
        self.index
    }
}

// ---- the provider ----

/// A face lookup's outcome, kept so a miss is not searched for twice.
type Picked = Option<Rc<dyn PdfFace>>;

/// The fonts of a set of directories.
pub struct FontSet {
    dirs: Vec<PathBuf>,
    index: OnceCell<Vec<Entry>>,
    picked: RefCell<HashMap<(String, bool, bool), Picked>>,
    loaded: RefCell<HashMap<(PathBuf, u32), Picked>>,
    fallbacks: OnceCell<Vec<Rc<dyn PdfFace>>>,
}

impl std::fmt::Debug for FontSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontSet").field("dirs", &self.dirs).finish()
    }
}

thread_local! {
    static SYSTEM: Rc<FontSet> = Rc::new(FontSet::system());
}

impl FontSet {
    /// The fonts installed on this machine.
    pub fn system() -> FontSet {
        FontSet::from_dirs(system_dirs())
    }

    /// This thread's shared system set, so the index is read once.
    pub fn shared() -> Rc<FontSet> {
        SYSTEM.with(Rc::clone)
    }

    /// The fonts under `dirs` (searched recursively).
    pub fn from_dirs(dirs: Vec<PathBuf>) -> FontSet {
        FontSet {
            dirs,
            index: OnceCell::new(),
            picked: RefCell::default(),
            loaded: RefCell::default(),
            fallbacks: OnceCell::new(),
        }
    }

    fn entries(&self) -> &[Entry] {
        self.index.get_or_init(|| {
            let mut out = Vec::new();
            for dir in &self.dirs {
                scan(dir, 0, &mut out);
            }
            out
        })
    }

    /// How many embeddable faces were found.
    pub fn len(&self) -> usize {
        self.entries().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The families found, sorted.
    pub fn families(&self) -> Vec<String> {
        let mut f: Vec<String> = self.entries().iter().map(|e| e.family.clone()).collect();
        f.sort();
        f.dedup();
        f
    }

    fn load(&self, entry: &Entry) -> Option<Rc<dyn PdfFace>> {
        self.loaded
            .borrow_mut()
            .entry((entry.path.clone(), entry.index))
            .or_insert_with(|| {
                SysFace::load(&entry.path, entry.index).map(|f| Rc::new(f) as Rc<dyn PdfFace>)
            })
            .clone()
    }

    /// The best face of `family` for the style: the style asked for, else the
    /// closest.
    fn best(&self, family: &str, bold: bool, italic: bool) -> Option<Rc<dyn PdfFace>> {
        let entry = self
            .entries()
            .iter()
            .filter(|e| e.family.eq_ignore_ascii_case(family))
            .max_by_key(|e| {
                (
                    u8::from(e.bold == bold) * 2 + u8::from(e.italic == italic),
                    std::cmp::Reverse((&e.path, e.index)),
                )
            })?;
        self.load(entry)
    }
}

impl PdfFontProvider for FontSet {
    fn face(&self, family: &str, bold: bool, italic: bool) -> Option<Rc<dyn PdfFace>> {
        let key = (family.trim().to_lowercase(), bold, italic);
        if let Some(hit) = self.picked.borrow().get(&key) {
            return hit.clone();
        }
        let wanted = family.trim();
        let mut tries: Vec<&str> = Vec::new();
        if !wanted.is_empty() {
            tries.push(wanted);
            tries.extend(aliases(&key.0));
        }
        tries.extend(DEFAULT_FAMILIES);
        let found = tries.into_iter().find_map(|f| self.best(f, bold, italic));
        self.picked.borrow_mut().insert(key, found.clone());
        found
    }

    fn fallbacks(&self) -> &[Rc<dyn PdfFace>] {
        self.fallbacks.get_or_init(|| {
            let mut out: Vec<Rc<dyn PdfFace>> = Vec::new();
            for family in FALLBACK_FAMILIES {
                if let Some(face) = self.best(family, false, false)
                    && !out.iter().any(|f| f.id() == face.id())
                {
                    out.push(face);
                }
            }
            out
        })
    }
}
