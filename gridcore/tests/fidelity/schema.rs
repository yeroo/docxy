//! Package and schema validation of saved SpreadsheetML (#1156): a
//! dependency-free check that a save is a package Excel opens without its
//! repair prompt, value loss or not. See "Package and schema validation" in
//! the `xlsx` section of `docs/fidelity-gate.md`.
//!
//! The package (OPC, ECMA-376 Part 2):
//!
//! - `missing-part`: no `[Content_Types].xml` or no `_rels/.rels`;
//! - `no-content-type`: a part with neither an `Override` nor a `Default` for
//!   its extension;
//! - `override-no-part`: an `Override` naming no part;
//! - `duplicate-part`: two entries naming one part;
//! - `dangling-target`: an internal relationship whose target is no part.
//!
//! Entry names, `Override` part names and relationship targets compare as
//! one normalized name ([`opc_name`]: `/` separators, percent-decoded, ASCII
//! case-insensitive). Which entries are parts is read from the ZIP central
//! directory as Excel
//! reads it: an entry is a directory when its name ends with `/` or its
//! external attributes say so (tdf124525.xlsx marks `_rels`, `xl`, ... only
//! there). The comparator's `read_parts` cannot decide this: it drops an empty
//! extensionless entry as the loader does, which is exactly the part a save
//! must not write.
//!
//! The content models (transitional `sml.xsd` of ECMA-376 Part 4) of every
//! part whose content type is XML, whatever its name, by child
//! name only, for `worksheet` (CT_Worksheet, `sheetData` required), its
//! `sheetData` (`row`s) and their `row`s (`c`s, `extLst`), and `workbook`
//! (CT_Workbook, `sheets` required): `not-allowed`, `order`, `duplicate` and
//! `missing` as in docx's `schema.rs`; and `r-order`, a `row` or `c` whose `r`
//! is not after its previous sibling's (one without `r` is skipped). Children
//! in another namespace (`mc:AlternateContent`, extensions) are not checked.

use std::collections::BTreeMap;

use super::comparator::{Elem, Node, parse_xml, read_parts, strip_indices};

const SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const SML_STRICT: &str = "http://purl.oclc.org/ooxml/spreadsheetml/main";
const CONTENT_TYPES: &str = "[Content_Types].xml";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    MissingPart,
    NoContentType,
    OverrideNoPart,
    DuplicatePart,
    DanglingTarget,
    NotAllowed,
    Order,
    Duplicate,
    Missing,
    ROrder,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::MissingPart => "missing-part",
            Rule::NoContentType => "no-content-type",
            Rule::OverrideNoPart => "override-no-part",
            Rule::DuplicatePart => "duplicate-part",
            Rule::DanglingTarget => "dangling-target",
            Rule::NotAllowed => "not-allowed",
            Rule::Order => "order",
            Rule::Duplicate => "duplicate",
            Rule::Missing => "missing",
            Rule::ROrder => "r-order",
        }
    }
}

/// One violation in one part (or, for the package rules, about one part).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// The part, by its OPC name in lower case.
    pub part: String,
    pub rule: Rule,
    /// The offending child, part name or target.
    pub child: String,
    /// The parent's path, with indices (`/worksheet/sheetData/row[3]`);
    /// empty for the package rules.
    pub parent: String,
}

impl Violation {
    /// What the gate counts: the same rule broken by the same child under the
    /// same (index-free) parent path in the same part.
    pub fn key(&self) -> (String, String, Rule, String) {
        (
            self.part.clone(),
            strip_indices(&self.parent),
            self.rule,
            self.child.clone(),
        )
    }

    pub fn line(&self, file: &str) -> String {
        format!(
            "SCHEMA {file} | {} | {} | {}/{}",
            self.part,
            self.rule.as_str(),
            self.parent,
            self.child
        )
    }
}

fn violation(part: &str, rule: Rule, child: &str, parent: &str) -> Violation {
    Violation {
        part: part.to_ascii_lowercase(),
        rule,
        child: child.to_string(),
        parent: parent.to_string(),
    }
}

// ---------------------------------------------------------------------------
// The package

/// The name and external attributes of each central directory entry, or
/// `None` when the central directory cannot be read.
fn central_entries(zip: &[u8]) -> Option<Vec<(String, u32)>> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(zip.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(zip.get(at..at + 4)?.try_into().ok()?));
    let u64_at = |at: usize| Some(u64::from_le_bytes(zip.get(at..at + 8)?.try_into().ok()?));
    let eocd = (0..=zip.len().checked_sub(22)?)
        .rev()
        .find(|&at| u32_at(at) == Some(0x0605_4b50))?;
    let (mut count, mut at) = (u64::from(u16_at(eocd + 10)?), u64::from(u32_at(eocd + 16)?));
    if at == 0xFFFF_FFFF || count == 0xFFFF {
        let locator = eocd.checked_sub(20)?;
        if u32_at(locator)? != 0x0706_4b50 {
            return None;
        }
        let eocd64 = usize::try_from(u64_at(locator + 8)?).ok()?;
        count = u64_at(eocd64 + 32)?;
        at = u64_at(eocd64 + 48)?;
    }
    let mut at = usize::try_from(at).ok()?;
    let mut out = Vec::new();
    for _ in 0..count {
        if u32_at(at)? != 0x0201_4b50 {
            return None;
        }
        let name_len = usize::from(u16_at(at + 28)?);
        let extra_len = usize::from(u16_at(at + 30)?);
        let comment_len = usize::from(u16_at(at + 32)?);
        let attrs = u32_at(at + 38)?;
        let name = zip.get(at + 46..at + 46 + name_len)?;
        out.push((String::from_utf8_lossy(name).into_owned(), attrs));
        at += 46 + name_len + extra_len + comment_len;
    }
    Some(out)
}

/// Whether a ZIP entry is a directory: its name ends with a separator, or its
/// external attributes carry the MS-DOS directory bit or a Unix directory
/// mode.
fn zip_directory(name: &str, attrs: u32) -> bool {
    name.ends_with('/')
        || name.ends_with('\\')
        || attrs & 0x10 != 0
        || (attrs >> 16) & 0o170_000 == 0o040_000
}

/// A part name as OPC compares it: `/` separators, no leading `/`, `%XX`
/// escapes decoded, ASCII lower case. ZIP entry names, `Override` part names
/// and relationship targets all go through it, so a part written
/// `xl/media/a%23.bin` is the one a target `media/a%23.bin` names.
fn opc_name(name: &str) -> String {
    percent_decode(&name.replace('\\', "/"))
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

/// `[Content_Types].xml`: the content type of each `Default` extension (in
/// lower case) and of each `Override` part (by [`opc_name`]).
struct ContentTypes {
    defaults: BTreeMap<String, String>,
    overrides: BTreeMap<String, String>,
}

impl ContentTypes {
    fn read(types: &Elem) -> Self {
        let mut defaults = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        for c in &types.children {
            let Node::Elem(e) = c else { continue };
            let get = |n: &str| e.attrs.iter().find(|a| a.local == n).map(|a| &a.value);
            let ct = get("ContentType").cloned().unwrap_or_default();
            match (e.local.as_str(), get("Extension"), get("PartName")) {
                ("Default", Some(x), _) => {
                    defaults.insert(x.to_ascii_lowercase(), ct);
                }
                ("Override", _, Some(p)) => {
                    overrides.insert(opc_name(p), ct);
                }
                _ => {}
            }
        }
        ContentTypes {
            defaults,
            overrides,
        }
    }

    /// The content type of the part `name` (by [`opc_name`]).
    fn of(&self, name: &str) -> Option<&str> {
        if let Some(ct) = self.overrides.get(name) {
            return Some(ct);
        }
        let leaf = name.rsplit('/').next().unwrap_or(name);
        let (_, ext) = leaf.rsplit_once('.')?;
        self.defaults.get(ext).map(String::as_str)
    }
}

/// `%XX` escapes decoded (a malformed one is kept as written).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The part (by [`opc_name`]) a relationship `target` of the part in `dir`
/// names: without a fragment, absolute or resolved against `dir`.
fn resolve(dir: &str, target: &str) -> String {
    let target = target.split('#').next().unwrap_or("").replace('\\', "/");
    let mut steps: Vec<&str> = match target.strip_prefix('/') {
        Some(_) => Vec::new(),
        None => dir.split('/').filter(|s| !s.is_empty()).collect(),
    };
    for step in target.split('/') {
        match step {
            "" | "." => {}
            ".." => {
                steps.pop();
            }
            s => steps.push(s),
        }
    }
    opc_name(&steps.join("/"))
}

/// The directory of the part whose relationships the part `rels` holds
/// (`xl/_rels/workbook.xml.rels` -> `xl`, `_rels/.rels` -> the root), or
/// `None` when `rels` is not in a `_rels` directory (in any case).
fn source_dir(rels: &str) -> Option<&str> {
    let (dir, _) = rels.rsplit_once('/')?;
    match dir.rsplit_once('/') {
        Some((parent, rels_dir)) if rels_dir.eq_ignore_ascii_case("_rels") => Some(parent),
        None if dir.eq_ignore_ascii_case("_rels") => Some(""),
        _ => None,
    }
}

/// The package's content types, if it has a readable `[Content_Types].xml`.
fn read_content_types(parts: &BTreeMap<String, Vec<u8>>) -> Option<ContentTypes> {
    parts
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(CONTENT_TYPES))
        .and_then(|(_, b)| parse_xml(b))
        .map(|types| ContentTypes::read(&types))
}

/// The package rules, over the parts the central directory names.
fn validate_opc(
    zip: &[u8],
    parts: &BTreeMap<String, Vec<u8>>,
    types: Option<&ContentTypes>,
) -> Vec<Violation> {
    let Some(entries) = central_entries(zip) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    for (name, attrs) in &entries {
        if !zip_directory(name, *attrs) {
            *names.entry(opc_name(name)).or_default() += 1;
        }
    }
    for (name, n) in &names {
        if *n > 1 {
            out.push(violation(name, Rule::DuplicatePart, name, ""));
        }
    }
    for required in [CONTENT_TYPES, "_rels/.rels"] {
        if !names.contains_key(&opc_name(required)) {
            out.push(violation(required, Rule::MissingPart, required, ""));
        }
    }
    if let Some(types) = types {
        for name in names.keys() {
            if name != &opc_name(CONTENT_TYPES) && types.of(name).is_none() {
                out.push(violation(name, Rule::NoContentType, name, ""));
            }
        }
        for name in types.overrides.keys() {
            if !names.contains_key(name) {
                out.push(violation(CONTENT_TYPES, Rule::OverrideNoPart, name, ""));
            }
        }
    }
    for (rels, bytes) in parts {
        if !rels.to_ascii_lowercase().ends_with(".rels") {
            continue;
        }
        let (Some(dir), Some(root)) = (source_dir(rels), parse_xml(bytes)) else {
            continue;
        };
        for c in &root.children {
            let Node::Elem(r) = c else { continue };
            let get = |n: &str| r.attrs.iter().find(|a| a.local == n).map(|a| &a.value);
            let external = get("TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External"));
            let Some(target) = get("Target") else {
                continue;
            };
            if r.local != "Relationship" || external {
                continue;
            }
            let part = resolve(dir, target);
            if !part.is_empty() && !names.contains_key(&part) {
                out.push(violation(rels, Rule::DanglingTarget, &part, ""));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Content models (ECMA-376 Part 4, transitional sml.xsd)

#[derive(Clone, Copy, PartialEq)]
enum Occurs {
    Optional,
    Required,
    Many,
}
use Occurs::{Many, Optional, Required};

type Slot = (&'static str, Occurs);

const WORKSHEET: &[Slot] = &[
    ("sheetPr", Optional),
    ("dimension", Optional),
    ("sheetViews", Optional),
    ("sheetFormatPr", Optional),
    ("cols", Many),
    ("sheetData", Required),
    ("sheetCalcPr", Optional),
    ("sheetProtection", Optional),
    ("protectedRanges", Optional),
    ("scenarios", Optional),
    ("autoFilter", Optional),
    ("sortState", Optional),
    ("dataConsolidate", Optional),
    ("customSheetViews", Optional),
    ("mergeCells", Optional),
    ("phoneticPr", Optional),
    ("conditionalFormatting", Many),
    ("dataValidations", Optional),
    ("hyperlinks", Optional),
    ("printOptions", Optional),
    ("pageMargins", Optional),
    ("pageSetup", Optional),
    ("headerFooter", Optional),
    ("rowBreaks", Optional),
    ("colBreaks", Optional),
    ("customProperties", Optional),
    ("cellWatches", Optional),
    ("ignoredErrors", Optional),
    ("smartTags", Optional),
    ("drawing", Optional),
    ("legacyDrawing", Optional),
    ("legacyDrawingHF", Optional),
    ("drawingHF", Optional),
    ("picture", Optional),
    ("oleObjects", Optional),
    ("controls", Optional),
    ("webPublishItems", Optional),
    ("tableParts", Optional),
    ("extLst", Optional),
];

const WORKBOOK: &[Slot] = &[
    ("fileVersion", Optional),
    ("fileSharing", Optional),
    ("workbookPr", Optional),
    ("workbookProtection", Optional),
    ("bookViews", Optional),
    ("sheets", Required),
    ("functionGroups", Optional),
    ("externalReferences", Optional),
    ("definedNames", Optional),
    ("calcPr", Optional),
    ("oleSize", Optional),
    ("customWorkbookViews", Optional),
    ("pivotCaches", Optional),
    ("smartTagPr", Optional),
    ("smartTagTypes", Optional),
    ("webPublishing", Optional),
    ("fileRecoveryPr", Many),
    ("webPublishObjects", Optional),
    ("extLst", Optional),
];

const SHEET_DATA: &[Slot] = &[("row", Many)];
const ROW: &[Slot] = &[("c", Many), ("extLst", Optional)];

fn is_sml(e: &Elem) -> bool {
    e.uri == SML || e.uri == SML_STRICT
}

/// The SpreadsheetML children of `e`, each with its path step (`row[3]`).
fn sml_children(e: &Elem) -> Vec<(&Elem, String)> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut out = Vec::new();
    for c in &e.children {
        let Node::Elem(c) = c else { continue };
        if !is_sml(c) {
            continue;
        }
        let n = seen.entry(c.local.as_str()).or_default();
        *n += 1;
        out.push((c, format!("{}[{n}]", c.local)));
    }
    out
}

fn check(part: &str, path: &str, e: &Elem, slots: &[Slot], out: &mut Vec<Violation>) {
    let mut cur = 0usize;
    let mut used = vec![0usize; slots.len()];
    for (c, _) in sml_children(e) {
        let name = c.local.as_str();
        let fits = |i: usize| slots[i].0 == name;
        let open = |i: usize, used: &[usize]| slots[i].1 == Many || used[i] == 0;
        if let Some(i) = (cur..slots.len()).find(|&i| fits(i) && open(i, &used)) {
            cur = i;
            used[i] += 1;
        } else if (0..slots.len()).any(|i| fits(i) && !open(i, &used)) {
            out.push(violation(part, Rule::Duplicate, name, path));
        } else if let Some(i) = (0..cur).find(|&i| fits(i)) {
            used[i] += 1;
            out.push(violation(part, Rule::Order, name, path));
        } else {
            out.push(violation(part, Rule::NotAllowed, name, path));
        }
    }
    for (i, (name, occurs)) in slots.iter().enumerate() {
        if *occurs == Required && used[i] == 0 {
            out.push(violation(part, Rule::Missing, name, path));
        }
    }
}

/// A row's `r`, or a cell's column from its `r` (`AB12` -> 28).
fn position(e: &Elem) -> Option<u64> {
    let r = &e
        .attrs
        .iter()
        .find(|a| a.local == "r" && a.uri.is_empty())?
        .value;
    if e.local == "row" {
        return r.trim().parse().ok();
    }
    let letters: String = r.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    if letters.is_empty() {
        return None;
    }
    letters.bytes().try_fold(0u64, |n, b| {
        n.checked_mul(26)?
            .checked_add(u64::from(b.to_ascii_uppercase() - b'A' + 1))
    })
}

/// `r-order`: each `row` (or `c`) of `e` must come after the previous one
/// that has an `r`.
fn check_r_order(part: &str, path: &str, e: &Elem, child: &str, out: &mut Vec<Violation>) {
    let mut last = None;
    for (c, _) in sml_children(e) {
        if c.local != child {
            continue;
        }
        let Some(at) = position(c) else { continue };
        if last.is_some_and(|l| at <= l) {
            out.push(violation(part, Rule::ROrder, child, path));
        }
        last = Some(at);
    }
}

/// Every content-model violation in one part's tree.
pub fn validate(part: &str, root: &Elem) -> Vec<Violation> {
    let mut out = Vec::new();
    if !is_sml(root) {
        return out;
    }
    let path = format!("/{}", root.local);
    match root.local.as_str() {
        "workbook" => check(part, &path, root, WORKBOOK, &mut out),
        "worksheet" => {
            check(part, &path, root, WORKSHEET, &mut out);
            for (data, step) in sml_children(root) {
                if data.local != "sheetData" {
                    continue;
                }
                let path = format!("{path}/{step}");
                check(part, &path, data, SHEET_DATA, &mut out);
                check_r_order(part, &path, data, "row", &mut out);
                for (row, step) in sml_children(data) {
                    if row.local != "row" {
                        continue;
                    }
                    let path = format!("{path}/{step}");
                    check(part, &path, row, ROW, &mut out);
                    check_r_order(part, &path, row, "c", &mut out);
                }
            }
        }
        _ => {}
    }
    out
}

/// Every violation of `pkg`: the package rules and the content models of its
/// XML parts.
pub fn validate_package(pkg: &[u8]) -> Vec<Violation> {
    let Some(parts) = read_parts(pkg) else {
        return Vec::new();
    };
    let types = read_content_types(&parts);
    let mut out = validate_opc(pkg, &parts, types.as_ref());
    for (name, bytes) in &parts {
        // An XML part by its content type, so a worksheet at `xl/sheet1`
        // with an Override is checked; by its name without content types.
        let xml = match &types {
            Some(types) => types
                .of(&opc_name(name))
                .is_some_and(|ct| ct.ends_with("xml")),
            None => name.to_ascii_lowercase().ends_with(".xml"),
        };
        if !xml {
            continue;
        }
        if let Some(root) = parse_xml(bytes) {
            out.extend(validate(name, &root));
        }
    }
    out
}

/// The violations `saved` adds over `original`: for each key, those beyond
/// the original's count. A violation the original has is not the writer's.
pub fn new_violations(original: &[Violation], saved: &[Violation]) -> Vec<Violation> {
    let mut budget: BTreeMap<_, usize> = BTreeMap::new();
    for v in original {
        *budget.entry(v.key()).or_default() += 1;
    }
    saved
        .iter()
        .filter(|v| match budget.get_mut(&v.key()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .cloned()
        .collect()
}
