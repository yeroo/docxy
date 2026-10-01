//! Document properties: the package's `docProps/core.xml` (Title, Author,
//! Created, Modified, …), `docProps/app.xml` (Company, Manager, Hyperlink
//! base, and the `TitlesOfParts` sheet list) and `docProps/custom.xml`
//! (user-defined name = value pairs).
//!
//! The parts stay the source of truth: reading parses them, and writing
//! **patches** them in place. A property that is set rewrites only its own
//! element, one that is cleared drops it, and everything the model does not
//! know (`cp:revision`, `AppVersion`, `DocSecurity`, extensions, a custom
//! property of a type we don't model) rides along byte for byte. A write
//! that changes nothing leaves the part untouched.
//!
//! A part that isn't UTF-8 XML we can read (UTF-16, truncated, malformed)
//! reads as empty and is never patched. Only setting custom properties
//! replaces an unreadable `custom.xml`, since that write says what the part
//! should hold. Hostile property parts never make a save fail.

use opccore::xml::{Event, XmlParser};

use crate::xlsx::{
    SheetPackage, add_content_type_override, add_rel, decode, esc_attr, esc_text,
    find_element_by_attr, local, override_element, parse_rels, resolve_relative,
};

const NS_CP: &str = "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
const NS_DCTERMS: &str = "http://purl.org/dc/terms/";
const NS_DCMITYPE: &str = "http://purl.org/dc/dcmitype/";
const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const NS_EXT: &str = "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties";
const NS_CUSTOM: &str = "http://schemas.openxmlformats.org/officeDocument/2006/custom-properties";
const NS_VT: &str = "http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes";

/// The `fmtid` every user-defined property carries.
pub const CUSTOM_FMTID: &str = "{D5CDD505-2E9C-101B-9397-08002B2CF9AE}";

/// The three property parts, each found through a package relationship.
#[derive(Clone, Copy, PartialEq)]
enum Part {
    Core,
    App,
    Custom,
}

impl Part {
    /// Relationship-type suffixes (Transitional, then Strict spelling).
    fn rel_suffixes(self) -> &'static [&'static str] {
        match self {
            Part::Core => &["/core-properties"],
            Part::App => &["/extended-properties", "/extendedProperties"],
            Part::Custom => &["/custom-properties", "/customProperties"],
        }
    }
    /// Where a part lives by convention, and where a new one is created.
    /// Excel writes these Transitional names in Strict packages too.
    fn default_name(self) -> &'static str {
        match self {
            Part::Core => "docProps/core.xml",
            Part::App => "docProps/app.xml",
            Part::Custom => "docProps/custom.xml",
        }
    }
    fn rel_type(self) -> &'static str {
        match self {
            Part::Core => {
                "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties"
            }
            Part::App => {
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties"
            }
            Part::Custom => {
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties"
            }
        }
    }
    fn content_type(self) -> &'static str {
        match self {
            Part::Core => "application/vnd.openxmlformats-package.core-properties+xml",
            Part::App => "application/vnd.openxmlformats-officedocument.extended-properties+xml",
            Part::Custom => "application/vnd.openxmlformats-officedocument.custom-properties+xml",
        }
    }
    fn template(self) -> String {
        let decl = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
        match self {
            Part::Core => format!(
                "{decl}<cp:coreProperties xmlns:cp=\"{NS_CP}\" xmlns:dc=\"{NS_DC}\" xmlns:dcterms=\"{NS_DCTERMS}\" xmlns:dcmitype=\"{NS_DCMITYPE}\" xmlns:xsi=\"{NS_XSI}\"></cp:coreProperties>"
            ),
            Part::App => {
                format!("{decl}<Properties xmlns=\"{NS_EXT}\" xmlns:vt=\"{NS_VT}\"></Properties>")
            }
            Part::Custom => {
                format!(
                    "{decl}<Properties xmlns=\"{NS_CUSTOM}\" xmlns:vt=\"{NS_VT}\"></Properties>"
                )
            }
        }
    }
}

/// The part `kind` names (resolved from the package root), and whether a
/// relationship says so. With no relationship, the conventional name.
fn locate(parts: &[(String, Vec<u8>)], kind: Part) -> (String, bool) {
    let rel = parts
        .iter()
        .find(|(n, _)| n == "_rels/.rels")
        .and_then(|(_, b)| {
            parse_rels(&String::from_utf8_lossy(b))
                .into_iter()
                .find(|(_, ty, _)| kind.rel_suffixes().iter().any(|s| ty.ends_with(s)))
                .map(|(_, _, t)| resolve_relative("", &t))
        });
    match rel {
        Some(name) => (name, true),
        None => (kind.default_name().to_string(), false),
    }
}

/// A property part as the package holds it.
enum PartXml {
    Missing,
    /// There, but not UTF-8 XML we can read: left exactly as it is.
    Unreadable,
    Xml(String),
}

fn part_xml(parts: &[(String, Vec<u8>)], name: &str) -> PartXml {
    let Some((_, bytes)) = parts.iter().find(|(n, _)| n == name) else {
        return PartXml::Missing;
    };
    // No lossy decoding: a UTF-16 part decoded as UTF-8 can still parse as
    // (garbage) XML, and patching that would destroy it.
    match std::str::from_utf8(bytes) {
        Ok(xml) if read_root(xml, 0).is_some() => PartXml::Xml(xml.to_string()),
        _ => PartXml::Unreadable,
    }
}

/// The text of a readable part.
fn part_text(parts: &[(String, Vec<u8>)], name: &str) -> Option<String> {
    match part_xml(parts, name) {
        PartXml::Xml(xml) => Some(xml),
        _ => None,
    }
}

/// The workbook's document properties. `None` (or an empty string, when
/// writing) means the element is absent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocProperties {
    /// `dc:title`
    pub title: Option<String>,
    /// `dc:subject`
    pub subject: Option<String>,
    /// `dc:creator` (Author)
    pub creator: Option<String>,
    /// `cp:keywords` (Tags)
    pub keywords: Option<String>,
    /// `dc:description` (Comments)
    pub description: Option<String>,
    /// `cp:lastModifiedBy`
    pub last_modified_by: Option<String>,
    /// `cp:category` (Categories)
    pub category: Option<String>,
    /// `dcterms:created`, as written (W3CDTF)
    pub created: Option<String>,
    /// `dcterms:modified`, as written (W3CDTF)
    pub modified: Option<String>,
    /// `app.xml` `Company`
    pub company: Option<String>,
    /// `app.xml` `Manager`
    pub manager: Option<String>,
    /// `app.xml` `HyperlinkBase`
    pub hyperlink_base: Option<String>,
    /// `custom.xml`, in file order.
    pub custom: Vec<CustomProperty>,
}

/// One user-defined property.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomProperty {
    pub name: String,
    pub value: CustomValue,
}

/// A custom property's value, by the variant type it is stored as.
#[derive(Debug, Clone, PartialEq)]
pub enum CustomValue {
    /// `vt:lpwstr` (also read from `vt:lpstr` / `vt:bstr`)
    Text(String),
    /// `vt:i4` when integral and in range, else `vt:r8` (any integer or
    /// real variant type reads as a number)
    Number(f64),
    /// `vt:bool`
    Bool(bool),
    /// `vt:filetime`, `YYYY-MM-DDTHH:MM:SSZ`
    Date(String),
    /// A type not modeled here: the property element's raw content, written
    /// back as it came.
    Other(String),
}

impl CustomValue {
    /// `text`, `number`, `bool`, `date` or `other`.
    pub fn type_name(&self) -> &'static str {
        match self {
            CustomValue::Text(_) => "text",
            CustomValue::Number(_) => "number",
            CustomValue::Bool(_) => "bool",
            CustomValue::Date(_) => "date",
            CustomValue::Other(_) => "other",
        }
    }

    /// The value as a person reads it (`Other` shows its raw XML).
    pub fn display(&self) -> String {
        match self {
            CustomValue::Text(s) | CustomValue::Date(s) | CustomValue::Other(s) => s.clone(),
            CustomValue::Number(n) => format_number(*n),
            CustomValue::Bool(b) => if *b { "Yes" } else { "No" }.to_string(),
        }
    }

    /// The value typed into a `Name = value` prompt: `true`/`false`/`yes`/
    /// `no` (any case) is a yes/no, a finite number is a number, a
    /// `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SSZ` date is a date, anything else
    /// is text. A leading `'` forces text (and is dropped).
    pub fn from_input(s: &str) -> CustomValue {
        if let Some(text) = s.strip_prefix('\'') {
            return CustomValue::Text(text.to_string());
        }
        let t = s.trim();
        match t.to_ascii_lowercase().as_str() {
            "true" | "yes" => return CustomValue::Bool(true),
            "false" | "no" => return CustomValue::Bool(false),
            _ => {}
        }
        if let Some(n) = parse_finite(t) {
            return CustomValue::Number(n);
        }
        if let Some(d) = normalize_date(t) {
            return CustomValue::Date(d);
        }
        CustomValue::Text(s.to_string())
    }
}

/// A finite number, spelled with digits (Rust also parses `inf`/`nan`,
/// which a property can't hold).
fn parse_finite(s: &str) -> Option<f64> {
    if !s.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SSZ` as the full W3CDTF form a
/// `vt:filetime` holds; `None` for anything else.
pub fn normalize_date(s: &str) -> Option<String> {
    let s = s.trim();
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let num = |r: std::ops::Range<usize>| s[r].parse::<u32>().ok();
    if b.len() < 10 || !digits(0..4) || b[4] != b'-' || !digits(5..7) || b[7] != b'-' {
        return None;
    }
    if !digits(8..10) {
        return None;
    }
    let (month, day) = (num(5..7)?, num(8..10)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    match b.len() {
        10 => Some(format!("{s}T00:00:00Z")),
        20 if b[10] == b'T'
            && digits(11..13)
            && b[13] == b':'
            && digits(14..16)
            && b[16] == b':'
            && digits(17..19)
            && b[19] == b'Z'
            && num(11..13)? < 24
            && num(14..16)? < 60
            && num(17..19)? < 60 =>
        {
            Some(s.to_string())
        }
        _ => None,
    }
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

// ---------------------------------------------------------------------------
// A light element tree over the part's text: spans, not values
// ---------------------------------------------------------------------------

/// One element and its direct children, as byte spans into the text it was
/// parsed from.
struct Elem {
    qname: String,
    /// Namespace URI of the element (from the declarations in scope).
    ns: String,
    start: usize,
    /// Just past the start tag's `>`.
    tag_end: usize,
    self_closing: bool,
    /// Where the end tag starts (`tag_end` when self-closing).
    close: usize,
    end: usize,
    children: Vec<Elem>,
    /// In-scope namespace declarations at the element: (prefix, uri), the
    /// default namespace with an empty prefix.
    decls: Vec<(String, String)>,
}

impl Elem {
    fn local(&self) -> &str {
        local(&self.qname)
    }
    /// The element's own text content, entities decoded.
    fn text(&self, xml: &str) -> String {
        if self.self_closing {
            return String::new();
        }
        let mut out = String::new();
        let mut p = XmlParser::new(&xml[self.tag_end..self.close]);
        loop {
            match p.next() {
                Event::Text => out.push_str(&decode(p.text())),
                Event::Eof => return out,
                _ => {}
            }
        }
    }
    /// The prefix (with its `:`) bound to `uri`, if one is.
    fn prefix_for(&self, uri: &str) -> Option<String> {
        self.decls.iter().find(|(_, u)| u == uri).map(|(p, _)| {
            if p.is_empty() {
                String::new()
            } else {
                format!("{p}:")
            }
        })
    }
    fn prefix(&self) -> &str {
        match self.qname.rfind(':') {
            Some(i) => &self.qname[..=i],
            None => "",
        }
    }
}

fn ns_of(p: &XmlParser, qname: &str) -> String {
    let want = match qname.rfind(':') {
        Some(i) => format!("xmlns:{}", &qname[..i]),
        None => "xmlns".to_string(),
    };
    p.namespace_attrs()
        .iter()
        .rev()
        .find(|a| a.name == want)
        .map(|a| decode(a.value))
        .unwrap_or_default()
}

/// The element the parser just started (`p` sits on its Start event),
/// with its children and grandchildren to `depth` levels.
fn read_elem(p: &mut XmlParser, xml: &str, depth: usize) -> Option<Elem> {
    let qname = p.name().to_string();
    let ns = ns_of(p, &qname);
    let decls = p
        .namespace_attrs()
        .iter()
        .map(|a| {
            let prefix = a.name.strip_prefix("xmlns").unwrap_or("");
            (prefix.trim_start_matches(':').to_string(), decode(a.value))
        })
        .collect();
    let start = p.start_pos();
    let tag_end = p.pos();
    let self_closing = xml[..tag_end].ends_with("/>");
    let mut children = Vec::new();
    let mut close = tag_end;
    loop {
        let before = p.pos();
        match p.next() {
            Event::Start if depth > 0 => children.push(read_elem(p, xml, depth - 1)?),
            Event::Start => {
                if !p.skip_element_complete() {
                    return None;
                }
            }
            Event::End => {
                if !self_closing {
                    close = before;
                }
                break;
            }
            Event::Eof => return None,
            Event::Text => {}
        }
    }
    if p.is_malformed() {
        return None;
    }
    Some(Elem {
        qname,
        ns,
        start,
        tag_end,
        self_closing,
        close,
        end: p.pos(),
        children,
        decls,
    })
}

/// The document element of a part, `depth` levels deep.
fn read_root(xml: &str, depth: usize) -> Option<Elem> {
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => return read_elem(&mut p, xml, depth),
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// Apply non-overlapping `(start, end, replacement)` edits to `xml`.
fn splice(xml: &str, mut ops: Vec<(usize, usize, String)>) -> String {
    ops.sort_by_key(|&(s, e, _)| (s, e));
    let mut out = String::with_capacity(xml.len() + 256);
    let mut at = 0usize;
    for (s, e, text) in ops {
        out.push_str(&xml[at..s]);
        out.push_str(&text);
        at = e;
    }
    out.push_str(&xml[at..]);
    out
}

/// One simple-text child element to set (`Some`, non-empty) or remove.
struct Field<'a> {
    ns: &'a str,
    local: &'a str,
    value: Option<&'a str>,
    /// A `dcterms:W3CDTF` date: a new element carries `xsi:type`.
    w3cdtf: bool,
}

/// Canonical prefixes for the namespaces a patch may need to declare.
fn canonical_prefix(uri: &str) -> &'static str {
    match uri {
        NS_CP => "cp",
        NS_DC => "dc",
        NS_DCTERMS => "dcterms",
        NS_XSI => "xsi",
        NS_VT => "vt",
        _ => "ns",
    }
}

/// Collects namespace declarations a patch adds to the root start tag.
struct Prefixes<'r> {
    root: &'r Elem,
    added: Vec<(String, String)>,
}

impl Prefixes<'_> {
    fn get(&mut self, uri: &str) -> String {
        if let Some(p) = self.root.prefix_for(uri) {
            return p;
        }
        if let Some((p, _)) = self.added.iter().find(|(_, u)| u == uri) {
            return format!("{p}:");
        }
        let base = canonical_prefix(uri);
        let taken = |p: &str| {
            self.root.decls.iter().any(|(d, _)| d == p) || self.added.iter().any(|(d, _)| d == p)
        };
        let mut prefix = base.to_string();
        let mut n = 1;
        while taken(&prefix) {
            prefix = format!("{base}{n}");
            n += 1;
        }
        self.added.push((prefix.clone(), uri.to_string()));
        format!("{prefix}:")
    }
    /// The ops that declare what was added (and close a self-closed root
    /// around `appended`).
    fn finish(self, xml: &str, appended: String, ops: &mut Vec<(usize, usize, String)>) {
        let decls: String = self
            .added
            .iter()
            .map(|(p, u)| format!(" xmlns:{p}=\"{}\"", esc_attr(u)))
            .collect();
        let root = self.root;
        if root.self_closing {
            if decls.is_empty() && appended.is_empty() {
                return;
            }
            let slash = xml[..root.tag_end].rfind("/>").unwrap_or(root.tag_end - 2);
            ops.push((
                slash,
                root.tag_end,
                format!("{decls}>{appended}</{}>", root.qname),
            ));
        } else {
            if !decls.is_empty() {
                ops.push((root.tag_end - 1, root.tag_end - 1, decls));
            }
            if !appended.is_empty() {
                ops.push((root.close, root.close, appended));
            }
        }
    }
}

/// Set or remove simple-text children of the document element, leaving
/// every other byte of the part as it is. `None` when the part can't be
/// read as XML (the caller leaves it alone).
fn patch_fields(xml: &str, fields: &[Field]) -> Option<String> {
    let root = read_root(xml, 1)?;
    let mut ops: Vec<(usize, usize, String)> = Vec::new();
    let mut prefixes = Prefixes {
        root: &root,
        added: Vec::new(),
    };
    let mut appended = String::new();
    for f in fields {
        let want = f.value.filter(|v| !v.is_empty());
        let found: Vec<&Elem> = root
            .children
            .iter()
            .filter(|c| c.ns == f.ns && c.local() == f.local)
            .collect();
        let current = found.first().map(|c| c.text(xml));
        if found.len() <= 1 && current.as_deref() == want {
            continue;
        }
        for dup in found.iter().skip(1) {
            ops.push((dup.start, dup.end, String::new()));
        }
        match (found.first(), want) {
            (Some(c), None) => ops.push((c.start, c.end, String::new())),
            // Only duplicates to drop.
            (Some(_), Some(v)) if current.as_deref() == Some(v) => {}
            (Some(c), Some(v)) if c.self_closing => {
                let tag = &xml[c.start..c.tag_end];
                let open = tag.strip_suffix("/>").unwrap_or(tag).trim_end();
                ops.push((
                    c.start,
                    c.end,
                    format!("{open}>{}</{}>", esc_text(v), c.qname),
                ));
            }
            (Some(c), Some(v)) => ops.push((c.tag_end, c.close, esc_text(v))),
            (None, Some(v)) => {
                let q = format!("{}{}", prefixes.get(f.ns), f.local);
                let attr = if f.w3cdtf {
                    let xsi = prefixes.get(NS_XSI);
                    let dcterms = prefixes.get(NS_DCTERMS);
                    format!(" {xsi}type=\"{dcterms}W3CDTF\"")
                } else {
                    String::new()
                };
                appended.push_str(&format!("<{q}{attr}>{}</{q}>", esc_text(v)));
            }
            (None, None) => {}
        }
    }
    if ops.is_empty() && appended.is_empty() {
        return Some(xml.to_string());
    }
    prefixes.finish(xml, appended, &mut ops);
    Some(splice(xml, ops))
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

fn read_core(xml: &str, props: &mut DocProperties) {
    let Some(root) = read_root(xml, 1) else {
        return;
    };
    for c in &root.children {
        let slot = match (c.ns.as_str(), c.local()) {
            (NS_DC, "title") => &mut props.title,
            (NS_DC, "subject") => &mut props.subject,
            (NS_DC, "creator") => &mut props.creator,
            (NS_CP, "keywords") => &mut props.keywords,
            (NS_DC, "description") => &mut props.description,
            (NS_CP, "lastModifiedBy") => &mut props.last_modified_by,
            (NS_CP, "category") => &mut props.category,
            (NS_DCTERMS, "created") => &mut props.created,
            (NS_DCTERMS, "modified") => &mut props.modified,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(c.text(xml)).filter(|s| !s.is_empty());
        }
    }
}

fn read_app(xml: &str, props: &mut DocProperties) {
    let Some(root) = read_root(xml, 1) else {
        return;
    };
    for c in &root.children {
        let slot = match c.local() {
            "Company" => &mut props.company,
            "Manager" => &mut props.manager,
            "HyperlinkBase" => &mut props.hyperlink_base,
            _ => continue,
        };
        if c.ns == root.ns && slot.is_none() {
            *slot = Some(c.text(xml)).filter(|s| !s.is_empty());
        }
    }
}

/// One `<property>` of custom.xml: its name, pid, value and span.
struct StoredProperty {
    name: String,
    pid: Option<u32>,
    value: CustomValue,
    start: usize,
    end: usize,
}

fn read_custom(xml: &str) -> Vec<StoredProperty> {
    let Some(root) = read_root(xml, 2) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for c in root.children.iter().filter(|c| c.local() == "property") {
        let tag = &xml[c.start..c.tag_end];
        let mut p = XmlParser::new(tag);
        // A property we can't name is not listed, so no edit touches it: it
        // stays in the part as it is.
        if p.next() != Event::Start || p.attr("name").is_empty() {
            continue;
        }
        let name = decode(p.attr("name"));
        let pid = p.attr("pid").trim().parse::<u32>().ok();
        let raw = || xml[c.tag_end..c.close].to_string();
        let value = match c.children.first() {
            None => CustomValue::Other(raw()),
            Some(v) => {
                let text = v.text(xml);
                match v.local() {
                    "lpwstr" | "lpstr" | "bstr" => CustomValue::Text(text),
                    "i1" | "i2" | "i4" | "i8" | "int" | "ui1" | "ui2" | "ui4" | "ui8" | "uint"
                    | "r4" | "r8" | "decimal" => match parse_finite(text.trim()) {
                        Some(n) => CustomValue::Number(n),
                        None => CustomValue::Other(raw()),
                    },
                    "bool" => match text.trim() {
                        "true" | "1" => CustomValue::Bool(true),
                        "false" | "0" => CustomValue::Bool(false),
                        _ => CustomValue::Other(raw()),
                    },
                    "filetime" | "date" => CustomValue::Date(text.trim().to_string()),
                    _ => CustomValue::Other(raw()),
                }
            }
        };
        out.push(StoredProperty {
            name,
            pid,
            value,
            start: c.start,
            end: c.end,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Write an empty part `kind` (adding it, or replacing an unreadable one)
/// with the package relationship and content type it needs; its name and
/// text.
fn create_part(parts: &mut Vec<(String, Vec<u8>)>, kind: Part) -> (String, String) {
    let (name, related) = locate(parts, kind);
    let xml = kind.template();
    match parts.iter_mut().find(|(n, _)| *n == name) {
        Some(p) => p.1 = xml.clone().into_bytes(),
        None => parts.push((name.clone(), xml.clone().into_bytes())),
    }
    if !related {
        add_rel(parts, "_rels/.rels", kind.rel_type(), &name);
    }
    add_content_type_override(parts, &format!("/{name}"), kind.content_type());
    (name, xml)
}

fn put(parts: &mut [(String, Vec<u8>)], name: &str, xml: String) {
    if let Some(p) = parts.iter_mut().find(|(n, _)| n == name) {
        if p.1 != xml.as_bytes() {
            p.1 = xml.into_bytes();
        }
    }
}

/// Drop part `name` with its package relationship and content type.
fn remove_part(parts: &mut Vec<(String, Vec<u8>)>, name: &str) {
    parts.retain(|(n, _)| n != name);
    if let Some(p) = parts.iter_mut().find(|(n, _)| n == "_rels/.rels") {
        let mut xml = String::from_utf8_lossy(&p.1).into_owned();
        while let Some(el) = find_element_by_attr(&xml, "Relationship", "Target", |t| {
            resolve_relative("", t) == name
        }) {
            xml.replace_range(el.start..el.end, "");
        }
        p.1 = xml.into_bytes();
    }
    if let Some(p) = parts.iter_mut().find(|(n, _)| n == "[Content_Types].xml") {
        let mut xml = String::from_utf8_lossy(&p.1).into_owned();
        while let Some(el) = override_element(&xml, &format!("/{name}")) {
            xml.replace_range(el.start..el.end, "");
        }
        p.1 = xml.into_bytes();
    }
}

fn set_fields(parts: &mut Vec<(String, Vec<u8>)>, kind: Part, fields: &[Field]) {
    let (name, _) = locate(parts, kind);
    let (name, xml) = match part_xml(parts, &name) {
        PartXml::Xml(xml) => (name, xml),
        PartXml::Unreadable => return,
        PartXml::Missing if fields.iter().all(|f| f.value.is_none_or(str::is_empty)) => return,
        PartXml::Missing => create_part(parts, kind),
    };
    if let Some(updated) = patch_fields(&xml, fields) {
        put(parts, &name, updated);
    }
}

fn custom_value_xml(vt: &str, value: &CustomValue) -> String {
    let el = |ty: &str, text: &str| format!("<{vt}{ty}>{text}</{vt}{ty}>");
    match value {
        CustomValue::Text(s) => el("lpwstr", &esc_text(s)),
        CustomValue::Number(n)
            if n.fract() == 0.0 && *n >= i32::MIN as f64 && *n <= i32::MAX as f64 =>
        {
            el("i4", &format!("{}", *n as i32))
        }
        CustomValue::Number(n) => el("r8", &format!("{n}")),
        CustomValue::Bool(b) => el("bool", if *b { "true" } else { "false" }),
        CustomValue::Date(d) => el("filetime", &esc_text(d)),
        CustomValue::Other(raw) => raw.clone(),
    }
}

/// A new `<property>` element for `prop`.
fn property_xml(prefixes: &mut Prefixes, own: &str, pid: u32, prop: &CustomProperty) -> String {
    let vt = prefixes.get(NS_VT);
    format!(
        "<{own}property fmtid=\"{CUSTOM_FMTID}\" pid=\"{pid}\" name=\"{}\">{}</{own}property>",
        esc_attr(&prop.name),
        custom_value_xml(&vt, &prop.value)
    )
}

/// Make custom.xml hold `custom`. A property whose name (any case) and value
/// are unchanged keeps its exact bytes; a changed one is rewritten in place
/// with its pid; a dropped one is cut; a new one is appended with the next
/// free pid. Whitespace, comments and elements we don't read stay. The part
/// goes away with its last property.
fn set_custom(parts: &mut Vec<(String, Vec<u8>)>, custom: &[CustomProperty]) {
    let (name, _) = locate(parts, Part::Custom);
    let (name, xml) = match part_xml(parts, &name) {
        PartXml::Xml(xml) => (name, xml),
        // Nothing to write; and an unreadable part may hold properties we
        // can't see, so only an explicit list replaces it.
        PartXml::Missing | PartXml::Unreadable if custom.is_empty() => return,
        PartXml::Missing | PartXml::Unreadable => create_part(parts, Part::Custom),
    };
    let Some(root) = read_root(&xml, 1) else {
        return;
    };
    let stored = read_custom(&xml);
    let unchanged = stored.len() == custom.len()
        && stored
            .iter()
            .zip(custom)
            .all(|(s, c)| s.name == c.name && s.value == c.value);
    if unchanged {
        return;
    }
    let all_listed = root
        .children
        .iter()
        .all(|c| stored.iter().any(|s| s.start == c.start));
    if custom.is_empty() && all_listed {
        remove_part(parts, &name);
        return;
    }

    let same = |a: &str, b: &str| a.to_lowercase() == b.to_lowercase();
    let own = root.prefix().to_string();
    let mut prefixes = Prefixes {
        root: &root,
        added: Vec::new(),
    };
    // Past every pid in the part, listed or not, so pids stay unique.
    let mut next_pid = root
        .children
        .iter()
        .filter_map(|c| {
            let mut p = XmlParser::new(&xml[c.start..c.tag_end]);
            (p.next() == Event::Start).then(|| p.attr("pid").trim().parse::<u32>().ok())?
        })
        .max()
        .unwrap_or(1)
        .clamp(1, u32::MAX - 1)
        + 1;
    let mut fresh_pid = || {
        let pid = next_pid;
        next_pid = next_pid.saturating_add(1);
        pid
    };
    let mut used = vec![false; stored.len()];
    let mut ops = Vec::new();
    let mut appended = String::new();
    for prop in custom {
        let found = (0..stored.len()).find(|&i| !used[i] && same(&stored[i].name, &prop.name));
        match found {
            Some(i) => {
                used[i] = true;
                let s = &stored[i];
                if s.name != prop.name || s.value != prop.value {
                    let pid = s.pid.unwrap_or_else(&mut fresh_pid);
                    ops.push((s.start, s.end, property_xml(&mut prefixes, &own, pid, prop)));
                }
            }
            None => {
                let pid = fresh_pid();
                appended.push_str(&property_xml(&mut prefixes, &own, pid, prop));
            }
        }
    }
    for (s, _) in stored.iter().zip(&used).filter(|(_, used)| !**used) {
        ops.push((s.start, s.end, String::new()));
    }
    prefixes.finish(&xml, appended, &mut ops);
    let updated = splice(&xml, ops);
    put(parts, &name, updated);
}

impl SheetPackage {
    /// The workbook's document properties, read from its property parts.
    pub fn doc_properties(&self) -> DocProperties {
        let mut props = DocProperties::default();
        if let Some(xml) = part_text(&self.parts, &locate(&self.parts, Part::Core).0) {
            read_core(&xml, &mut props);
        }
        if let Some(xml) = part_text(&self.parts, &locate(&self.parts, Part::App).0) {
            read_app(&xml, &mut props);
        }
        if let Some(xml) = part_text(&self.parts, &locate(&self.parts, Part::Custom).0) {
            props.custom = read_custom(&xml)
                .into_iter()
                .map(|s| CustomProperty {
                    name: s.name,
                    value: s.value,
                })
                .collect();
        }
        props
    }

    /// Write `props` into the property parts. Only what differs from the
    /// parts is touched; a part is created (with its relationship and
    /// content type) only when there is something to put in it, and
    /// `custom.xml` goes away with its last property.
    pub fn set_doc_properties(&mut self, props: &DocProperties) {
        fn f<'a>(ns: &'a str, local: &'a str, value: &'a Option<String>) -> Field<'a> {
            Field {
                ns,
                local,
                value: value.as_deref(),
                w3cdtf: false,
            }
        }
        fn date<'a>(local: &'a str, value: &'a Option<String>) -> Field<'a> {
            Field {
                w3cdtf: true,
                ..f(NS_DCTERMS, local, value)
            }
        }
        // Excel's element order, for whatever has to be appended.
        let core = [
            f(NS_DC, "title", &props.title),
            f(NS_DC, "subject", &props.subject),
            f(NS_DC, "creator", &props.creator),
            f(NS_CP, "keywords", &props.keywords),
            f(NS_DC, "description", &props.description),
            f(NS_CP, "lastModifiedBy", &props.last_modified_by),
            date("created", &props.created),
            date("modified", &props.modified),
            f(NS_CP, "category", &props.category),
        ];
        set_fields(&mut self.parts, Part::Core, &core);

        // app.xml's children are in its root's namespace (Transitional or
        // Strict); a new part is Transitional.
        let (app_name, _) = locate(&self.parts, Part::App);
        let app_ns = part_text(&self.parts, &app_name)
            .and_then(|xml| read_root(&xml, 0).map(|r| r.ns))
            .unwrap_or_else(|| NS_EXT.to_string());
        let app = [
            f(&app_ns, "Manager", &props.manager),
            f(&app_ns, "Company", &props.company),
            f(&app_ns, "HyperlinkBase", &props.hyperlink_base),
        ];
        set_fields(&mut self.parts, Part::App, &app);

        set_custom(&mut self.parts, &props.custom);
    }

    /// Stamp a save: `dcterms:modified` = `now` (W3CDTF) and
    /// `cp:lastModifiedBy` = `user`. A package with no core properties yet
    /// also gets `user` as its author and `now` as its creation time. The
    /// clock is an argument so the stamp is the caller's (and a test's)
    /// choice; [`crate::xlsx::save_xlsx`] itself never stamps.
    ///
    /// Only core.xml is written (app.xml and custom.xml are not looked at),
    /// and an unreadable core.xml is left as it is.
    pub fn stamp_save(&mut self, now: &str, user: &str) {
        let (core, _) = locate(&self.parts, Part::Core);
        let fresh = matches!(part_xml(&self.parts, &core), PartXml::Missing);
        let (now, user) = (Some(now.to_string()), Some(user.to_string()));
        let mut fields = vec![
            Field {
                ns: NS_CP,
                local: "lastModifiedBy",
                value: user.as_deref(),
                w3cdtf: false,
            },
            Field {
                ns: NS_DCTERMS,
                local: "modified",
                value: now.as_deref(),
                w3cdtf: true,
            },
        ];
        if fresh {
            fields.insert(
                0,
                Field {
                    ns: NS_DC,
                    local: "creator",
                    value: user.as_deref(),
                    w3cdtf: false,
                },
            );
            fields.insert(
                2,
                Field {
                    ns: NS_DCTERMS,
                    local: "created",
                    value: now.as_deref(),
                    w3cdtf: true,
                },
            );
        }
        set_fields(&mut self.parts, Part::Core, &fields);
    }
}

// ---------------------------------------------------------------------------
// TitlesOfParts / HeadingPairs
// ---------------------------------------------------------------------------

/// Group names Excel gives the non-worksheet parts, so the first-group
/// fallback (for a localized `Worksheets`) never picks one of them.
const OTHER_GROUPS: &[&str] = &[
    "charts",
    "named ranges",
    "excel 4.0 macros",
    "international macro sheets",
    "dialog sheets",
    "modules",
];

/// One `HeadingPairs` group: its name variant (raw) and count.
struct Group {
    name: String,
    name_raw: String,
    count: usize,
    count_raw: String,
}

/// `tag` (a start tag) with attribute `name` set to `value`.
fn set_attr(tag: &str, name: &str, value: &str) -> String {
    let mut p = XmlParser::new(tag);
    if p.next() == Event::Start {
        if let Some(a) = p.attrs().iter().find(|a| local(a.name) == name) {
            let at = a.value.as_ptr() as usize - tag.as_ptr() as usize;
            return format!("{}{value}{}", &tag[..at], &tag[at + a.value.len()..]);
        }
    }
    let cut = if tag.ends_with("/>") {
        tag.len() - 2
    } else {
        tag.len() - 1
    };
    format!("{} {name}=\"{value}\"{}", &tag[..cut], &tag[cut..])
}

/// `app.xml` with the worksheet group of `TitlesOfParts`/`HeadingPairs`
/// listing `worksheets`, and an existing `Charts` group listing `charts`.
/// `None` when nothing changes or the lists can't be read consistently.
fn refresh_titles(xml: &str, worksheets: &[String], charts: &[String]) -> Option<String> {
    let root = read_root(xml, 4)?;
    let child = |n: &str| root.children.iter().find(|c| c.local() == n);
    let pairs = child("HeadingPairs")?.children.first()?;
    let titles = child("TitlesOfParts")?.children.first()?;
    if pairs.local() != "vector" || titles.local() != "vector" || pairs.children.len() % 2 != 0 {
        return None;
    }
    let mut groups = Vec::new();
    for pair in pairs.children.chunks(2) {
        let name = pair[0].children.first()?.text(xml);
        let count = pair[1].children.first()?.text(xml).trim().parse().ok()?;
        groups.push(Group {
            name,
            name_raw: xml[pair[0].start..pair[0].end].to_string(),
            count,
            count_raw: xml[pair[1].start..pair[1].end].to_string(),
        });
    }
    // Counts come from the file: a huge one must not overflow the sum (and
    // wrap round to match) or run past the titles.
    let total = groups
        .iter()
        .try_fold(0usize, |sum, g| sum.checked_add(g.count))?;
    if total != titles.children.len() {
        return None;
    }
    let ws_group = groups
        .iter()
        .position(|g| g.name.eq_ignore_ascii_case("Worksheets"))
        .or_else(|| {
            let first = groups.first()?;
            let other = OTHER_GROUPS
                .iter()
                .any(|o| first.name.eq_ignore_ascii_case(o));
            (!other).then_some(0)
        });
    let chart_group = groups
        .iter()
        .position(|g| g.name.eq_ignore_ascii_case("Charts"));
    if ws_group.is_none() && chart_group.is_none() {
        return None;
    }

    // Each group's entries: the original ones, or the model's names.
    let mut entries: Vec<Vec<Result<&str, String>>> = Vec::new();
    let mut at = 0usize;
    let mut changed = false;
    for (i, g) in groups.iter().enumerate() {
        let original = titles.children.get(at..at.checked_add(g.count)?)?;
        at += g.count;
        let names = if Some(i) == ws_group {
            Some(worksheets)
        } else if Some(i) == chart_group {
            Some(charts)
        } else {
            None
        };
        match names {
            Some(names) => {
                let same = names.len() == original.len()
                    && names.iter().zip(original).all(|(n, e)| *n == e.text(xml));
                if same {
                    entries.push(original.iter().map(|e| Ok(&xml[e.start..e.end])).collect());
                } else {
                    changed = true;
                    entries.push(names.iter().map(|n| Err(n.clone())).collect());
                }
            }
            None => entries.push(original.iter().map(|e| Ok(&xml[e.start..e.end])).collect()),
        }
    }
    if !changed {
        return None;
    }

    let vt = titles.prefix();
    let mut pair_body = String::new();
    let mut title_body = String::new();
    let mut kept_groups = 0;
    let mut kept_titles = 0;
    for (g, list) in groups.iter().zip(&entries) {
        if list.is_empty() {
            continue;
        }
        kept_groups += 1;
        kept_titles += list.len();
        pair_body.push_str(&g.name_raw);
        if list.len() == g.count {
            pair_body.push_str(&g.count_raw);
        } else {
            let vp = pairs.prefix();
            pair_body.push_str(&format!(
                "<{vp}variant><{vp}i4>{}</{vp}i4></{vp}variant>",
                list.len()
            ));
        }
        for e in list {
            match e {
                Ok(raw) => title_body.push_str(raw),
                Err(name) => {
                    title_body.push_str(&format!("<{vt}lpstr>{}</{vt}lpstr>", esc_text(name)))
                }
            }
        }
    }
    let vector = |v: &Elem, size: usize, body: String| {
        let tag = set_attr(&xml[v.start..v.tag_end], "size", &size.to_string());
        let open = if v.self_closing {
            format!("{}>", tag.strip_suffix("/>").unwrap_or(&tag).trim_end())
        } else {
            tag
        };
        format!("{open}{body}</{}>", v.qname)
    };
    let ops = vec![
        (
            pairs.start,
            pairs.end,
            vector(pairs, kept_groups * 2, pair_body),
        ),
        (
            titles.start,
            titles.end,
            vector(titles, kept_titles, title_body),
        ),
    ];
    Some(splice(xml, ops))
}

/// Bring `app.xml`'s sheet list in line with the model's sheets (in tab
/// order, hidden ones included). Called on every save; it writes only when
/// the names differ, so an unmodified workbook saves byte for byte.
pub(crate) fn refresh_titles_of_parts(parts: &mut [(String, Vec<u8>)], pkg: &SheetPackage) {
    let (name, _) = locate(parts, Part::App);
    let Some(xml) = part_text(parts, &name) else {
        return;
    };
    let kinds = pkg.sheet_rel_types();
    let mut worksheets = Vec::new();
    let mut charts = Vec::new();
    for (i, sheet) in pkg.workbook.sheets.iter().enumerate() {
        match kinds.get(i).map(String::as_str) {
            Some(ty) if ty.ends_with("/chartsheet") => charts.push(sheet.name.clone()),
            Some(ty) if ty.is_empty() || ty.ends_with("/worksheet") => {
                worksheets.push(sheet.name.clone())
            }
            Some(_) => {}
            None => worksheets.push(sheet.name.clone()),
        }
    }
    if let Some(updated) = refresh_titles(&xml, &worksheets, &charts) {
        put(parts, &name, updated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xlsx::{load_xlsx, new_xlsx, save_xlsx};

    fn text(pkg: &SheetPackage, name: &str) -> String {
        String::from_utf8_lossy(pkg.part(name).unwrap()).into_owned()
    }

    const CORE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:creator>Author One</dc:creator><cp:lastModifiedBy>Author Two</cp:lastModifiedBy><cp:revision>7</cp:revision><dcterms:created xsi:type="dcterms:W3CDTF">2020-01-02T03:04:05Z</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">2020-06-07T08:09:10Z</dcterms:modified><cp:contentStatus>Draft</cp:contentStatus></cp:coreProperties>"#;

    const APP: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>Microsoft Excel</Application><DocSecurity>0</DocSecurity><ScaleCrop>false</ScaleCrop><HeadingPairs><vt:vector size="4" baseType="variant"><vt:variant><vt:lpstr>Worksheets</vt:lpstr></vt:variant><vt:variant><vt:i4>2</vt:i4></vt:variant><vt:variant><vt:lpstr>Named Ranges</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant></vt:vector></HeadingPairs><TitlesOfParts><vt:vector size="3" baseType="lpstr"><vt:lpstr>Alpha</vt:lpstr><vt:lpstr>Beta</vt:lpstr><vt:lpstr>Total</vt:lpstr></vt:vector></TitlesOfParts><AppVersion>16.0300</AppVersion></Properties>"#;

    /// A two-sheet workbook (Alpha, Beta) with Excel-shaped core and app
    /// parts, related from the package root.
    fn with_props(core: &str, app: &str) -> SheetPackage {
        let mut pkg = new_xlsx();
        pkg.rename_sheet(0, "Alpha");
        pkg.add_sheet("Beta");
        let mut pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
        pkg.set_part("docProps/core.xml", core.as_bytes().to_vec());
        pkg.set_part("docProps/app.xml", app.as_bytes().to_vec());
        add_rel(
            &mut pkg.parts,
            "_rels/.rels",
            Part::Core.rel_type(),
            "docProps/core.xml",
        );
        add_rel(
            &mut pkg.parts,
            "_rels/.rels",
            Part::App.rel_type(),
            "docProps/app.xml",
        );
        load_xlsx(&save_xlsx(&pkg)).unwrap()
    }

    #[test]
    fn reads_core_properties_by_namespace_not_prefix() {
        let core = r#"<p:coreProperties xmlns:p="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:e="http://purl.org/dc/elements/1.1/" xmlns:t="http://purl.org/dc/terms/"><e:title>A &amp; B</e:title><t:title>not this</t:title><e:creator>Me</e:creator><p:keywords>k1; k2</p:keywords><t:created>2020-01-02T03:04:05Z</t:created></p:coreProperties>"#;
        let pkg = with_props(core, APP);
        let p = pkg.doc_properties();
        assert_eq!(p.title.as_deref(), Some("A & B"));
        assert_eq!(p.creator.as_deref(), Some("Me"));
        assert_eq!(p.keywords.as_deref(), Some("k1; k2"));
        assert_eq!(p.created.as_deref(), Some("2020-01-02T03:04:05Z"));
        assert_eq!(p.subject, None);
    }

    #[test]
    fn every_property_round_trips_through_save_and_load() {
        let mut pkg = with_props(CORE, APP);
        let mut p = pkg.doc_properties();
        p.title = Some("T <&>\"' title".into());
        p.subject = Some("Subj".into());
        p.keywords = Some("tag1 tag2".into());
        p.category = Some("Cat".into());
        p.description = Some("Line one\nline two".into());
        p.company = Some("Acme & Co".into());
        p.manager = Some("Boss".into());
        p.hyperlink_base = Some("https://example.com/base/".into());
        p.custom = vec![
            CustomProperty {
                name: "Client".into(),
                value: CustomValue::Text("Contoso <x>".into()),
            },
            CustomProperty {
                name: "Count".into(),
                value: CustomValue::Number(42.0),
            },
            CustomProperty {
                name: "Ratio".into(),
                value: CustomValue::Number(0.25),
            },
            CustomProperty {
                name: "Done".into(),
                value: CustomValue::Bool(true),
            },
            CustomProperty {
                name: "Due".into(),
                value: CustomValue::Date("2024-05-06T00:00:00Z".into()),
            },
        ];
        pkg.set_doc_properties(&p);
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(re.doc_properties(), p);

        let core = text(&re, "docProps/core.xml");
        assert!(core.contains("<dc:title>T &lt;&amp;&gt;\"' title</dc:title>"));
        assert!(core.contains("<cp:keywords>tag1 tag2</cp:keywords>"));
        assert!(core.contains("<cp:category>Cat</cp:category>"));
        assert!(core.contains("<dc:description>Line one\nline two</dc:description>"));
        // Unknown elements ride along.
        assert!(core.contains("<cp:revision>7</cp:revision>"));
        assert!(core.contains("<cp:contentStatus>Draft</cp:contentStatus>"));
        let app = text(&re, "docProps/app.xml");
        assert!(app.contains("<Company>Acme &amp; Co</Company>"));
        assert!(app.contains("<Manager>Boss</Manager>"));
        assert!(app.contains("<HyperlinkBase>https://example.com/base/</HyperlinkBase>"));
        assert!(app.contains("<AppVersion>16.0300</AppVersion>"));
        assert!(app.contains("<DocSecurity>0</DocSecurity>"));

        let custom = text(&re, "docProps/custom.xml");
        assert!(custom.contains(&format!(
            r#"<property fmtid="{CUSTOM_FMTID}" pid="2" name="Client"><vt:lpwstr>Contoso &lt;x&gt;</vt:lpwstr></property>"#
        )));
        assert!(custom.contains(r#"pid="3" name="Count"><vt:i4>42</vt:i4>"#));
        assert!(custom.contains(r#"pid="4" name="Ratio"><vt:r8>0.25</vt:r8>"#));
        assert!(custom.contains(r#"pid="5" name="Done"><vt:bool>true</vt:bool>"#));
        assert!(
            custom
                .contains(r#"pid="6" name="Due"><vt:filetime>2024-05-06T00:00:00Z</vt:filetime>"#)
        );
        let rels = text(&re, "_rels/.rels");
        assert!(rels.contains(Part::Custom.rel_type()));
        assert!(text(&re, "[Content_Types].xml").contains(
            r#"<Override PartName="/docProps/custom.xml" ContentType="application/vnd.openxmlformats-officedocument.custom-properties+xml"/>"#
        ));
    }

    #[test]
    fn an_empty_value_removes_the_element() {
        let mut pkg = with_props(CORE, APP);
        let mut p = pkg.doc_properties();
        p.title = Some("x".into());
        pkg.set_doc_properties(&p);
        assert!(text(&pkg, "docProps/core.xml").contains("<dc:title>x</dc:title>"));
        p.title = Some(String::new());
        p.last_modified_by = None;
        pkg.set_doc_properties(&p);
        let core = text(&pkg, "docProps/core.xml");
        assert!(!core.contains("title"));
        assert!(!core.contains("lastModifiedBy"));
        assert!(core.contains("<dc:creator>Author One</dc:creator>"));
    }

    #[test]
    fn an_unchanged_write_leaves_the_parts_alone() {
        let mut pkg = with_props(CORE, APP);
        let before = save_xlsx(&pkg);
        let p = pkg.doc_properties();
        pkg.set_doc_properties(&p);
        assert_eq!(save_xlsx(&pkg), before);
    }

    #[test]
    fn a_self_closed_element_and_root_take_a_value() {
        let core = r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title/></cp:coreProperties>"#;
        let mut pkg = with_props(
            core,
            r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"/>"#,
        );
        let mut p = pkg.doc_properties();
        p.title = Some("New".into());
        p.company = Some("Co".into());
        p.modified = Some("2024-01-01T00:00:00Z".into());
        pkg.set_doc_properties(&p);
        let core = text(&pkg, "docProps/core.xml");
        assert!(core.contains("<dc:title>New</dc:title>"));
        // dcterms and xsi were not declared: the patch declares them.
        assert!(core.contains(r#"xmlns:dcterms="http://purl.org/dc/terms/""#));
        assert!(core.contains(r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#));
        assert!(core.contains(
            r#"<dcterms:modified xsi:type="dcterms:W3CDTF">2024-01-01T00:00:00Z</dcterms:modified>"#
        ));
        let app = text(&pkg, "docProps/app.xml");
        assert!(
            app.ends_with("><Company>Co</Company></Properties>"),
            "{app}"
        );
        assert_eq!(pkg.doc_properties(), p);
    }

    #[test]
    fn stamp_keeps_the_author_and_creation_time() {
        let mut pkg = with_props(CORE, APP);
        pkg.stamp_save("2026-10-01T12:00:00Z", "editor");
        let p = pkg.doc_properties();
        assert_eq!(p.creator.as_deref(), Some("Author One"));
        assert_eq!(p.created.as_deref(), Some("2020-01-02T03:04:05Z"));
        assert_eq!(p.last_modified_by.as_deref(), Some("editor"));
        assert_eq!(p.modified.as_deref(), Some("2026-10-01T12:00:00Z"));
        let core = text(&pkg, "docProps/core.xml");
        assert!(core.contains(
            r#"<dcterms:modified xsi:type="dcterms:W3CDTF">2026-10-01T12:00:00Z</dcterms:modified>"#
        ));
        assert!(core.contains("<cp:revision>7</cp:revision>"));
    }

    #[test]
    fn stamp_creates_core_properties_when_there_are_none() {
        let mut pkg = new_xlsx();
        pkg.stamp_save("2026-10-01T12:00:00Z", "me");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let p = re.doc_properties();
        assert_eq!(p.creator.as_deref(), Some("me"));
        assert_eq!(p.last_modified_by.as_deref(), Some("me"));
        assert_eq!(p.created.as_deref(), Some("2026-10-01T12:00:00Z"));
        assert_eq!(p.modified.as_deref(), Some("2026-10-01T12:00:00Z"));
        assert!(text(&re, "_rels/.rels").contains(
            r#"Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml""#
        ));
        assert!(text(&re, "[Content_Types].xml").contains(
            r#"<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>"#
        ));
        // Nothing asked for app.xml or custom.xml.
        assert!(re.part("docProps/app.xml").is_none());
        assert!(re.part("docProps/custom.xml").is_none());
    }

    #[test]
    fn app_properties_create_app_xml_when_absent() {
        let mut pkg = new_xlsx();
        let p = DocProperties {
            company: Some("Acme".into()),
            ..DocProperties::default()
        };
        pkg.set_doc_properties(&p);
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(re.doc_properties().company.as_deref(), Some("Acme"));
        assert!(text(&re, "_rels/.rels").contains(
            r#"Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml""#
        ));
        assert!(text(&re, "[Content_Types].xml").contains(
            r#"<Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>"#
        ));
        assert!(re.part("docProps/core.xml").is_none());
    }

    #[test]
    fn parts_are_found_through_their_relationships() {
        let mut pkg = new_xlsx();
        pkg.set_part("meta/props.xml", CORE.as_bytes().to_vec());
        pkg.set_part(
            "meta/x.xml",
            br#"<Properties xmlns="http://purl.oclc.org/ooxml/officeDocument/extendedProperties"><Company>Strict Co</Company></Properties>"#.to_vec(),
        );
        add_rel(
            &mut pkg.parts,
            "_rels/.rels",
            Part::Core.rel_type(),
            "meta/props.xml",
        );
        add_rel(
            &mut pkg.parts,
            "_rels/.rels",
            "http://purl.oclc.org/ooxml/officeDocument/relationships/extendedProperties",
            "/meta/x.xml",
        );
        let mut p = pkg.doc_properties();
        assert_eq!(p.creator.as_deref(), Some("Author One"));
        assert_eq!(p.company.as_deref(), Some("Strict Co"));
        p.company = Some("Other".into());
        p.manager = Some("M".into());
        pkg.set_doc_properties(&p);
        assert!(pkg.part("docProps/app.xml").is_none());
        assert_eq!(pkg.doc_properties(), p);
        assert!(text(&pkg, "meta/x.xml").contains("<Manager>M</Manager>"));
    }

    fn custom_pkg(body: &str) -> SheetPackage {
        let mut pkg = new_xlsx();
        pkg.set_part(
            "docProps/custom.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes">{body}</Properties>"#
            )
            .into_bytes(),
        );
        add_rel(
            &mut pkg.parts,
            "_rels/.rels",
            Part::Custom.rel_type(),
            "docProps/custom.xml",
        );
        add_content_type_override(
            &mut pkg.parts,
            "/docProps/custom.xml",
            Part::Custom.content_type(),
        );
        pkg
    }

    const OTHER: &str = r#"<property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="5" name="Blob" linkTarget="x"><vt:clsid>{00000000-0000-0000-0000-000000000001}</vt:clsid></property>"#;

    #[test]
    fn unmodeled_custom_values_survive_other_edits() {
        let mut pkg = custom_pkg(&format!(
            "\n  <property fmtid=\"{CUSTOM_FMTID}\" pid=\"2\" name=\"A\"><vt:lpwstr>a</vt:lpwstr></property>\n  <!-- kept -->{OTHER}\n  <property fmtid=\"{CUSTOM_FMTID}\" pid=\"9\" name=\"B\"><vt:i8>12</vt:i8></property>\n"
        ));
        let mut p = pkg.doc_properties();
        assert_eq!(p.custom.len(), 3);
        assert!(matches!(p.custom[1].value, CustomValue::Other(_)));
        assert_eq!(p.custom[2].value, CustomValue::Number(12.0));

        // An unrelated property edit: custom.xml is untouched.
        let before = text(&pkg, "docProps/custom.xml");
        p.title = Some("t".into());
        pkg.set_doc_properties(&p);
        assert_eq!(text(&pkg, "docProps/custom.xml"), before);

        // Editing another custom property keeps the Other one, its order
        // and pid; a new one takes the next free pid.
        p.custom[0].value = CustomValue::Text("changed".into());
        p.custom.push(CustomProperty {
            name: "C".into(),
            value: CustomValue::Bool(false),
        });
        pkg.set_doc_properties(&p);
        let xml = text(&pkg, "docProps/custom.xml");
        let a = xml.find(r#"pid="2" name="A"><vt:lpwstr>changed"#).unwrap();
        let blob = xml.find(OTHER).unwrap();
        let b = xml.find(r#"pid="9" name="B"><vt:i8>12</vt:i8>"#).unwrap();
        let c = xml.find(r#"pid="10" name="C"><vt:bool>false"#).unwrap();
        assert!(a < blob && blob < b && b < c, "{xml}");
        assert!(xml.contains("</property>\n  <!-- kept -->"), "{xml}");
        assert!(
            xml.contains("<vt:i8>12</vt:i8></property>\n<property"),
            "{xml}"
        );
        assert_eq!(pkg.doc_properties(), p);
    }

    /// A custom.xml we can't read: `bytes` as the part, related and typed.
    fn raw_custom_pkg(bytes: Vec<u8>) -> SheetPackage {
        let mut pkg = custom_pkg("");
        pkg.set_part("docProps/custom.xml", bytes);
        pkg
    }

    fn utf16(xml: &str) -> Vec<u8> {
        let mut out = vec![0xFF, 0xFE];
        for u in xml.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    }

    #[test]
    fn an_unreadable_custom_xml_survives_saves_and_other_edits() {
        let valid = format!(
            r#"<?xml version="1.0" encoding="UTF-16"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="{CUSTOM_FMTID}" pid="2" name="A"><vt:lpwstr>a</vt:lpwstr></property></Properties>"#
        );
        let truncated = format!(
            r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties"><property fmtid="{CUSTOM_FMTID}" pid="2" name="A"><vt:lpwstr>a</vt"#
        );
        for bytes in [utf16(&valid), truncated.into_bytes()] {
            let mut pkg = raw_custom_pkg(bytes.clone());
            assert!(pkg.doc_properties().custom.is_empty());
            pkg.stamp_save("2026-10-01T12:00:00Z", "me");
            let mut re = load_xlsx(&save_xlsx(&pkg)).unwrap();
            assert_eq!(re.part("docProps/custom.xml").unwrap(), bytes.as_slice());
            assert!(text(&re, "_rels/.rels").contains("custom-properties"));

            // An unrelated edit leaves it alone too.
            let mut p = re.doc_properties();
            p.title = Some("T".into());
            re.set_doc_properties(&p);
            assert_eq!(re.part("docProps/custom.xml").unwrap(), bytes.as_slice());
            assert_eq!(re.doc_properties().title.as_deref(), Some("T"));

            // Setting custom properties replaces it with what was set.
            p.custom = vec![CustomProperty {
                name: "B".into(),
                value: CustomValue::Bool(true),
            }];
            re.set_doc_properties(&p);
            assert_eq!(re.doc_properties(), p);
            let re = load_xlsx(&save_xlsx(&re)).unwrap();
            assert_eq!(re.doc_properties().custom, p.custom);
        }
    }

    #[test]
    fn a_utf16_core_xml_is_never_patched() {
        let bytes = utf16(CORE);
        let mut pkg = with_props(CORE, APP);
        pkg.set_part("docProps/core.xml", bytes.clone());
        pkg.stamp_save("2026-10-01T12:00:00Z", "me");
        let mut p = pkg.doc_properties();
        assert_eq!(p.creator, None);
        p.title = Some("T".into());
        pkg.set_doc_properties(&p);
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(re.part("docProps/core.xml").unwrap(), bytes.as_slice());
    }

    #[test]
    fn a_property_without_a_name_is_kept_as_it_is() {
        let nameless = r#"<property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="3"><vt:lpwstr>x</vt:lpwstr></property>"#;
        let mut pkg = custom_pkg(&format!(
            r#"<property fmtid="{CUSTOM_FMTID}" pid="2" name="A"><vt:lpwstr>a</vt:lpwstr></property>{nameless}"#
        ));
        let mut p = pkg.doc_properties();
        assert_eq!(p.custom.len(), 1);
        // Removing the only listed property keeps the part for the other.
        p.custom.clear();
        pkg.set_doc_properties(&p);
        let xml = text(&pkg, "docProps/custom.xml");
        assert!(xml.contains(nameless), "{xml}");
        assert!(!xml.contains(r#"name="A""#), "{xml}");
        // A new one takes a pid after every pid in the part.
        p.custom.push(CustomProperty {
            name: "C".into(),
            value: CustomValue::Text("c".into()),
        });
        pkg.set_doc_properties(&p);
        assert!(text(&pkg, "docProps/custom.xml").contains(r#"pid="4" name="C""#));
    }

    #[test]
    fn an_unchanged_custom_list_leaves_the_part_alone() {
        let body = format!(
            "\n <property fmtid=\"{CUSTOM_FMTID}\" pid=\"2\" name=\"A\"><vt:lpwstr>a</vt:lpwstr></property> <!-- c -->\n"
        );
        let mut pkg = custom_pkg(&body);
        let before = text(&pkg, "docProps/custom.xml");
        let p = pkg.doc_properties();
        pkg.set_doc_properties(&p);
        assert_eq!(text(&pkg, "docProps/custom.xml"), before);
    }

    #[test]
    fn removing_the_last_custom_property_removes_the_part() {
        let mut pkg = custom_pkg(&format!(
            r#"<property fmtid="{CUSTOM_FMTID}" pid="2" name="A"><vt:lpwstr>a</vt:lpwstr></property>"#
        ));
        let mut p = pkg.doc_properties();
        p.custom.clear();
        pkg.set_doc_properties(&p);
        assert!(pkg.part("docProps/custom.xml").is_none());
        assert!(!text(&pkg, "_rels/.rels").contains("custom-properties"));
        assert!(!text(&pkg, "[Content_Types].xml").contains("custom.xml"));
        load_xlsx(&save_xlsx(&pkg)).unwrap();
    }

    #[test]
    fn custom_numbers_pick_i4_or_r8() {
        let vt = "vt:";
        assert_eq!(
            custom_value_xml(vt, &CustomValue::Number(-7.0)),
            "<vt:i4>-7</vt:i4>"
        );
        assert_eq!(
            custom_value_xml(vt, &CustomValue::Number(3e9)),
            "<vt:r8>3000000000</vt:r8>"
        );
        assert_eq!(
            custom_value_xml(vt, &CustomValue::Number(1.5)),
            "<vt:r8>1.5</vt:r8>"
        );
    }

    #[test]
    fn prompt_input_infers_the_type() {
        use CustomValue::*;
        assert_eq!(CustomValue::from_input("Yes"), Bool(true));
        assert_eq!(CustomValue::from_input("FALSE"), Bool(false));
        assert_eq!(CustomValue::from_input("no"), Bool(false));
        assert_eq!(CustomValue::from_input("12.5"), Number(12.5));
        assert_eq!(CustomValue::from_input("-3"), Number(-3.0));
        assert_eq!(CustomValue::from_input("inf"), Text("inf".into()));
        assert_eq!(CustomValue::from_input("NaN"), Text("NaN".into()));
        assert_eq!(
            CustomValue::from_input("2024-05-06"),
            Date("2024-05-06T00:00:00Z".into())
        );
        assert_eq!(
            CustomValue::from_input("2024-05-06T07:08:09Z"),
            Date("2024-05-06T07:08:09Z".into())
        );
        assert_eq!(
            CustomValue::from_input("2024-13-06"),
            Text("2024-13-06".into())
        );
        assert_eq!(CustomValue::from_input("'42"), Text("42".into()));
        assert_eq!(CustomValue::from_input("'yes"), Text("yes".into()));
        assert_eq!(CustomValue::from_input("hello"), Text("hello".into()));
    }

    // --- TitlesOfParts ------------------------------------------------------

    fn titles(pkg: &SheetPackage) -> String {
        let app = text(pkg, "docProps/app.xml");
        let a = app.find("<HeadingPairs>").unwrap();
        let b = app.find("</TitlesOfParts>").unwrap() + "</TitlesOfParts>".len();
        app[a..b].to_string()
    }

    #[test]
    fn titles_of_parts_follow_renames_adds_and_removes() {
        let mut pkg = with_props(CORE, APP);
        pkg.rename_sheet(1, "Gamma");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(
            titles(&re),
            r#"<HeadingPairs><vt:vector size="4" baseType="variant"><vt:variant><vt:lpstr>Worksheets</vt:lpstr></vt:variant><vt:variant><vt:i4>2</vt:i4></vt:variant><vt:variant><vt:lpstr>Named Ranges</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant></vt:vector></HeadingPairs><TitlesOfParts><vt:vector size="3" baseType="lpstr"><vt:lpstr>Alpha</vt:lpstr><vt:lpstr>Gamma</vt:lpstr><vt:lpstr>Total</vt:lpstr></vt:vector></TitlesOfParts>"#
        );

        let mut pkg = re;
        pkg.add_sheet("D & E");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert!(titles(&re).contains(r#"<vt:i4>3</vt:i4>"#));
        assert!(titles(&re).contains(
            r#"<vt:vector size="4" baseType="lpstr"><vt:lpstr>Alpha</vt:lpstr><vt:lpstr>Gamma</vt:lpstr><vt:lpstr>D &amp; E</vt:lpstr><vt:lpstr>Total</vt:lpstr>"#
        ));

        let mut pkg = re;
        pkg.remove_sheet(0);
        pkg.remove_sheet(0);
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(
            titles(&re),
            r#"<HeadingPairs><vt:vector size="4" baseType="variant"><vt:variant><vt:lpstr>Worksheets</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant><vt:variant><vt:lpstr>Named Ranges</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant></vt:vector></HeadingPairs><TitlesOfParts><vt:vector size="2" baseType="lpstr"><vt:lpstr>D &amp; E</vt:lpstr><vt:lpstr>Total</vt:lpstr></vt:vector></TitlesOfParts>"#
        );
    }

    #[test]
    fn titles_of_parts_untouched_when_names_match() {
        // `&apos;` and whitespace between entries: equal names, so the part
        // keeps its exact bytes.
        let app = APP
            .replace(
                "<vt:lpstr>Beta</vt:lpstr>",
                "\n  <vt:lpstr>Be&#116;a</vt:lpstr>\n",
            )
            .replace("Total", "O&apos;Brien");
        let pkg = with_props(CORE, &app);
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(text(&re, "docProps/app.xml"), app);
    }

    #[test]
    fn titles_of_parts_first_group_fallback_for_a_localized_name() {
        let app = APP.replace(
            "<vt:lpstr>Worksheets</vt:lpstr>",
            "<vt:lpstr>Arbeitsblätter</vt:lpstr>",
        );
        let mut pkg = with_props(CORE, &app);
        pkg.rename_sheet(0, "Eins");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert!(titles(&re).contains(
            "<vt:lpstr>Eins</vt:lpstr><vt:lpstr>Beta</vt:lpstr><vt:lpstr>Total</vt:lpstr>"
        ));
        assert!(titles(&re).contains("Arbeitsblätter"));
    }

    #[test]
    fn titles_of_parts_left_alone_when_counts_disagree() {
        let app = APP.replace("<vt:i4>1</vt:i4>", "<vt:i4>5</vt:i4>");
        let mut pkg = with_props(CORE, &app);
        pkg.rename_sheet(0, "Zed");
        let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
        assert_eq!(text(&re, "docProps/app.xml"), app);
    }

    #[test]
    fn titles_of_parts_survive_an_overflowing_count() {
        for (counts, titles) in [
            (
                ["18446744073709551615", "1"],
                r#"<vt:vector size="0" baseType="lpstr"/>"#,
            ),
            (
                ["18446744073709551615", "2"],
                r#"<vt:vector size="1" baseType="lpstr"><vt:lpstr>Alpha</vt:lpstr></vt:vector>"#,
            ),
        ] {
            let app = format!(
                r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><HeadingPairs><vt:vector size="4" baseType="variant"><vt:variant><vt:lpstr>Worksheets</vt:lpstr></vt:variant><vt:variant><vt:i4>{}</vt:i4></vt:variant><vt:variant><vt:lpstr>Named Ranges</vt:lpstr></vt:variant><vt:variant><vt:i4>{}</vt:i4></vt:variant></vt:vector></HeadingPairs><TitlesOfParts>{titles}</TitlesOfParts></Properties>"#,
                counts[0], counts[1]
            );
            let mut pkg = with_props(CORE, &app);
            pkg.rename_sheet(0, "Zed");
            let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
            assert_eq!(text(&re, "docProps/app.xml"), app);
        }
    }

    #[test]
    fn chart_sheets_go_to_the_charts_group_only() {
        let mut pkg = with_props(CORE, APP);
        // Turn sheet 2 (Beta) into a chartsheet: its workbook relationship
        // says so, and app.xml has a Charts group for it.
        let rels = text(&pkg, "xl/_rels/workbook.xml.rels");
        let target = "worksheets/sheet2.xml";
        let at = rels.find(&format!("Target=\"{target}\"")).unwrap();
        let open = rels[..at].rfind("<Relationship").unwrap();
        let fixed = format!(
            "{}{}",
            &rels[..open],
            rels[open..].replacen("/worksheet\"", "/chartsheet\"", 1)
        );
        pkg.set_part("xl/_rels/workbook.xml.rels", fixed.into_bytes());
        let app = r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><HeadingPairs><vt:vector size="4" baseType="variant"><vt:variant><vt:lpstr>Worksheets</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant><vt:variant><vt:lpstr>Charts</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant></vt:vector></HeadingPairs><TitlesOfParts><vt:vector size="2" baseType="lpstr"><vt:lpstr>Alpha</vt:lpstr><vt:lpstr>Beta</vt:lpstr></vt:vector></TitlesOfParts></Properties>"#;
        pkg.set_part("docProps/app.xml", app.as_bytes().to_vec());
        assert_eq!(
            text(&load_xlsx(&save_xlsx(&pkg)).unwrap(), "docProps/app.xml"),
            app
        );

        pkg.rename_sheet(1, "Chart1");
        pkg.add_sheet("Gamma");
        let out = text(&load_xlsx(&save_xlsx(&pkg)).unwrap(), "docProps/app.xml");
        assert!(out.contains(
            r#"<vt:variant><vt:lpstr>Worksheets</vt:lpstr></vt:variant><vt:variant><vt:i4>2</vt:i4></vt:variant><vt:variant><vt:lpstr>Charts</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant>"#
        ), "{out}");
        assert!(out.contains(
            r#"<vt:vector size="3" baseType="lpstr"><vt:lpstr>Alpha</vt:lpstr><vt:lpstr>Gamma</vt:lpstr><vt:lpstr>Chart1</vt:lpstr></vt:vector>"#
        ), "{out}");
    }
}
