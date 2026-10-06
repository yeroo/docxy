//! Round-trip fidelity gate (#1060): a canonical XML comparator for OPC
//! packages, plus the allowlist and the shrink-only baseline it is judged
//! against. See `docs/fidelity-gate.md`.
//!
//! Lives in the test crate so `docxcore` itself stays std-only and free of
//! test-only code; it is built on the same `opccore` XML pull parser and ZIP
//! reader the crate uses.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};

use docxcore::xml::{Event, XmlParser};
use docxcore::zip::ZipArchive;

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Above this many LCS cells, child alignment falls back to pairing in order.
const LCS_CELL_LIMIT: usize = 4_000_000;

// ---------------------------------------------------------------------------
// Findings

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    LostElement,
    LostAttr,
    ChangedValue,
    ExtraElement,
    ExtraAttr,
    PartMissing,
    PartExtra,
    PartBytes,
    LoadError,
    Panic,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::LostElement,
        Kind::LostAttr,
        Kind::ChangedValue,
        Kind::ExtraElement,
        Kind::ExtraAttr,
        Kind::PartMissing,
        Kind::PartExtra,
        Kind::PartBytes,
        Kind::LoadError,
        Kind::Panic,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::LostElement => "lost-element",
            Kind::LostAttr => "lost-attr",
            Kind::ChangedValue => "changed-value",
            Kind::ExtraElement => "extra-element",
            Kind::ExtraAttr => "extra-attr",
            Kind::PartMissing => "part-missing",
            Kind::PartExtra => "part-extra",
            Kind::PartBytes => "part-bytes",
            Kind::LoadError => "load-error",
            Kind::Panic => "panic",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// One difference between an original part and its saved counterpart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub part: String,
    pub kind: Kind,
    /// XPath-like location with sibling indices, e.g. `/w:document/w:body/w:p[3]/w:pPr/w:foo`.
    pub path: String,
    /// The detail shown in the report (old and new value, panic message, ...).
    pub detail: String,
}

impl Finding {
    fn new(part: &str, kind: Kind, path: String, detail: String) -> Self {
        Finding {
            part: part.to_string(),
            kind,
            path,
            detail,
        }
    }

    /// The path without sibling indices: the baseline key granularity.
    pub fn key_path(&self) -> String {
        strip_indices(&self.path)
    }
}

/// `/w:body/w:p[3]/w:r[12]/@w:val` -> `/w:body/w:p/w:r/@w:val`.
pub fn strip_indices(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_index = false;
    for c in path.chars() {
        match c {
            '[' => in_index = true,
            ']' if in_index => in_index = false,
            _ if !in_index => out.push(c),
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Canonical DOM

#[derive(Clone, Debug)]
pub struct Elem {
    pub uri: String,
    pub local: String,
    /// The name as written, for the report.
    pub qname: String,
    /// Non-namespace-declaration attributes, sorted by (uri, local).
    pub attrs: Vec<Attr>,
    pub children: Vec<Node>,
    /// Hash of the canonical subtree (prefix-, order- and whitespace-normalized).
    pub hash: u64,
}

#[derive(Clone, Debug)]
pub struct Attr {
    pub uri: String,
    pub local: String,
    pub qname: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub enum Node {
    Elem(Elem),
    Text(String),
}

impl Node {
    fn hash(&self) -> u64 {
        match self {
            Node::Elem(e) => e.hash,
            Node::Text(t) => {
                let mut h = DefaultHasher::new();
                1u8.hash(&mut h);
                t.hash(&mut h);
                h.finish()
            }
        }
    }

    /// The alignment key when subtrees differ: expanded name, or text.
    fn name_key(&self) -> (&str, &str) {
        match self {
            Node::Elem(e) => (&e.uri, &e.local),
            Node::Text(_) => ("", "#text"),
        }
    }
}

fn resolve(parser: &XmlParser<'_>, qname: &str, is_attr: bool) -> (String, String) {
    let (prefix, local) = match qname.split_once(':') {
        Some((p, l)) => (Some(p), l),
        None => (None, qname),
    };
    let uri = match prefix {
        Some("xml") => XML_NS.to_string(),
        Some(p) => parser
            .namespace_attrs()
            .iter()
            .find(|a| a.name.strip_prefix("xmlns:") == Some(p))
            .map(|a| decode(a.value))
            .unwrap_or_else(|| format!("unbound:{p}")),
        // Unprefixed attributes are in no namespace; elements take the default.
        None if is_attr => String::new(),
        None => parser
            .namespace_attrs()
            .iter()
            .find(|a| a.name == "xmlns")
            .map(|a| decode(a.value))
            .unwrap_or_default(),
    };
    (uri, local.to_string())
}

fn decode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    XmlParser::append_decoded(raw, &mut out);
    out
}

struct Building {
    elem: Elem,
    text: String,
}

fn flush_text(b: &mut Building) {
    if !b.text.is_empty() {
        b.elem
            .children
            .push(Node::Text(std::mem::take(&mut b.text)));
    }
}

fn finish(mut b: Building) -> Elem {
    flush_text(&mut b);
    let mut e = b.elem;
    // Whitespace-only text between elements is indentation, not content. An
    // element whose only content is whitespace (`<w:t> </w:t>`) keeps it.
    if e.children.iter().any(|c| matches!(c, Node::Elem(_))) {
        e.children
            .retain(|c| !matches!(c, Node::Text(t) if t.chars().all(char::is_whitespace)));
    }
    let mut h = DefaultHasher::new();
    0u8.hash(&mut h);
    e.uri.hash(&mut h);
    e.local.hash(&mut h);
    for a in &e.attrs {
        a.uri.hash(&mut h);
        a.local.hash(&mut h);
        a.value.hash(&mut h);
    }
    for c in &e.children {
        c.hash().hash(&mut h);
    }
    e.hash = h.finish();
    e
}

/// Parse an XML part into its canonical DOM. `None` when the part is not
/// UTF-8 or is malformed; the caller then compares bytes instead.
pub fn parse_xml(bytes: &[u8]) -> Option<Elem> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).ok()?;
    let mut parser = XmlParser::new(text);
    let mut stack: Vec<Building> = Vec::new();
    let mut root = None;
    loop {
        match parser.next() {
            Event::Start => {
                if let Some(top) = stack.last_mut() {
                    flush_text(top);
                } else if root.is_some() {
                    return None; // a second root element
                }
                let qname = parser.name();
                let (uri, local) = resolve(&parser, qname, false);
                let mut attrs: Vec<Attr> = parser
                    .attrs()
                    .iter()
                    .filter(|a| a.name != "xmlns" && !a.name.starts_with("xmlns:"))
                    .map(|a| {
                        let (uri, local) = resolve(&parser, a.name, true);
                        Attr {
                            uri,
                            local,
                            qname: a.name.to_string(),
                            value: decode(a.value),
                        }
                    })
                    .collect();
                attrs.sort_by(|a, b| (&a.uri, &a.local).cmp(&(&b.uri, &b.local)));
                stack.push(Building {
                    elem: Elem {
                        uri,
                        local,
                        qname: qname.to_string(),
                        attrs,
                        children: Vec::new(),
                        hash: 0,
                    },
                    text: String::new(),
                });
            }
            Event::End => {
                let open = stack.pop()?;
                if open.elem.qname != parser.name() {
                    return None; // mismatched end tag
                }
                let done = finish(open);
                match stack.last_mut() {
                    Some(parent) => parent.elem.children.push(Node::Elem(done)),
                    None => root = Some(done),
                }
            }
            Event::Text => {
                if let Some(top) = stack.last_mut() {
                    if parser.is_cdata() {
                        top.text.push_str(parser.text());
                    } else {
                        XmlParser::append_decoded(parser.text(), &mut top.text);
                    }
                }
            }
            Event::Eof => break,
        }
    }
    if parser.is_malformed() || !stack.is_empty() {
        return None;
    }
    root
}

// ---------------------------------------------------------------------------
// Comparison

/// Compare two canonical trees, reporting every difference once, at the root
/// of the differing subtree.
pub fn compare_xml(part: &str, original: &Elem, saved: &Elem) -> Vec<Finding> {
    let mut out = Vec::new();
    let root = format!("/{}", original.qname);
    if (&original.uri, &original.local) != (&saved.uri, &saved.local) {
        out.push(Finding::new(part, Kind::LostElement, root, String::new()));
        out.push(Finding::new(
            part,
            Kind::ExtraElement,
            format!("/{}", saved.qname),
            String::new(),
        ));
        return out;
    }
    compare_elem(part, &root, original, saved, &mut out);
    out
}

fn compare_elem(part: &str, path: &str, a: &Elem, b: &Elem, out: &mut Vec<Finding>) {
    if a.hash == b.hash {
        return;
    }
    for attr in &a.attrs {
        let at = format!("{path}/@{}", attr.qname);
        match b
            .attrs
            .iter()
            .find(|x| x.uri == attr.uri && x.local == attr.local)
        {
            None => out.push(Finding::new(part, Kind::LostAttr, at, attr.value.clone())),
            Some(x) if x.value != attr.value => out.push(Finding::new(
                part,
                Kind::ChangedValue,
                at,
                format!("{:?} -> {:?}", attr.value, x.value),
            )),
            Some(_) => {}
        }
    }
    for attr in &b.attrs {
        if !a
            .attrs
            .iter()
            .any(|x| x.uri == attr.uri && x.local == attr.local)
        {
            out.push(Finding::new(
                part,
                Kind::ExtraAttr,
                format!("{path}/@{}", attr.qname),
                attr.value.clone(),
            ));
        }
    }

    let a_steps = child_steps(path, &a.children);
    let b_steps = child_steps(path, &b.children);
    for (ai, bi) in align(&a.children, &b.children) {
        match (ai, bi) {
            (Some(i), Some(j)) => match (&a.children[i], &b.children[j]) {
                (Node::Elem(x), Node::Elem(y)) => compare_elem(part, &a_steps[i], x, y, out),
                (Node::Text(x), Node::Text(y)) if x != y => out.push(Finding::new(
                    part,
                    Kind::ChangedValue,
                    a_steps[i].clone(),
                    format!("{x:?} -> {y:?}"),
                )),
                _ => {}
            },
            (Some(i), None) => out.push(Finding::new(
                part,
                Kind::LostElement,
                a_steps[i].clone(),
                summary(&a.children[i]),
            )),
            (None, Some(j)) => out.push(Finding::new(
                part,
                Kind::ExtraElement,
                b_steps[j].clone(),
                summary(&b.children[j]),
            )),
            (None, None) => {}
        }
    }
}

/// The detail of a lost or extra child: a text's value, or an element's
/// attributes (sorted, as written), which allowlist rules can match on.
fn summary(n: &Node) -> String {
    match n {
        Node::Text(t) => format!("{t:?}"),
        Node::Elem(e) => e
            .attrs
            .iter()
            .map(|a| format!("{}={:?}", a.qname, a.value))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// The XPath step of each child: `qname`, or `qname[n]` when the parent has
/// several children of that name; `text()` for text.
fn child_steps(path: &str, children: &[Node]) -> Vec<String> {
    let mut totals: HashMap<&str, usize> = HashMap::new();
    for c in children {
        if let Node::Elem(e) = c {
            *totals.entry(&e.qname).or_default() += 1;
        }
    }
    let mut seen: HashMap<&str, usize> = HashMap::new();
    children
        .iter()
        .map(|c| match c {
            Node::Text(_) => format!("{path}/text()"),
            Node::Elem(e) => {
                let n = seen.entry(&e.qname).or_default();
                *n += 1;
                if totals[e.qname.as_str()] > 1 {
                    format!("{path}/{}[{n}]", e.qname)
                } else {
                    format!("{path}/{}", e.qname)
                }
            }
        })
        .collect()
}

/// Align two child lists: identical subtrees anchor first, then the gaps
/// between anchors align by expanded name. Each output pair is
/// (original index, saved index); `None` on one side is a loss or an extra.
/// Order is significant (schema order), so a moved child is a loss plus an
/// extra.
pub fn align(a: &[Node], b: &[Node]) -> Vec<(Option<usize>, Option<usize>)> {
    let ah: Vec<u64> = a.iter().map(Node::hash).collect();
    let bh: Vec<u64> = b.iter().map(Node::hash).collect();
    let anchors = lcs(&ah, &bh);
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (ai, bj) in anchors.into_iter().chain([(a.len(), b.len())]) {
        let ak: Vec<_> = a[i..ai].iter().map(Node::name_key).collect();
        let bk: Vec<_> = b[j..bj].iter().map(Node::name_key).collect();
        let pairs = lcs(&ak, &bk);
        let (mut x, mut y) = (0, 0);
        for (px, py) in pairs.into_iter().chain([(ak.len(), bk.len())]) {
            out.extend((x..px).map(|k| (Some(i + k), None)));
            out.extend((y..py).map(|k| (None, Some(j + k))));
            if px < ak.len() {
                out.push((Some(i + px), Some(j + py)));
            }
            x = px + 1;
            y = py + 1;
        }
        if ai < a.len() {
            out.push((Some(ai), Some(bj)));
        }
        i = ai + 1;
        j = bj + 1;
    }
    out
}

/// Longest common subsequence of two key sequences, as matched index pairs.
/// Common prefix and suffix are matched directly; above [`LCS_CELL_LIMIT`]
/// the middle pairs equal keys greedily in order instead.
fn lcs<T: PartialEq>(a: &[T], b: &[T]) -> Vec<(usize, usize)> {
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut out: Vec<(usize, usize)> = (0..pre).map(|k| (k, k)).collect();

    if am.len().saturating_mul(bm.len()) > LCS_CELL_LIMIT {
        let mut j = 0;
        for (i, x) in am.iter().enumerate() {
            if let Some(off) = bm[j..].iter().position(|y| y == x) {
                out.push((pre + i, pre + j + off));
                j += off + 1;
            }
        }
    } else if !am.is_empty() && !bm.is_empty() {
        let (n, m) = (am.len(), bm.len());
        // table[i][j] = LCS length of am[i..], bm[j..].
        let mut table = vec![0u32; (n + 1) * (m + 1)];
        let at = |i: usize, j: usize| i * (m + 1) + j;
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                table[at(i, j)] = if am[i] == bm[j] {
                    table[at(i + 1, j + 1)] + 1
                } else {
                    table[at(i + 1, j)].max(table[at(i, j + 1)])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if am[i] == bm[j] {
                out.push((pre + i, pre + j));
                i += 1;
                j += 1;
            } else if table[at(i + 1, j)] >= table[at(i, j + 1)] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    out.extend((0..suf).map(|k| (a.len() - suf + k, b.len() - suf + k)));
    out
}

// ---------------------------------------------------------------------------
// Packages

fn read_parts(bytes: &[u8]) -> Option<BTreeMap<String, Vec<u8>>> {
    let zip = ZipArchive::open(bytes)?;
    let mut parts = BTreeMap::new();
    for e in zip.entries() {
        if e.name.ends_with('/') {
            continue;
        }
        parts.insert(e.name.clone(), zip.extract(e)?);
    }
    Some(parts)
}

/// Part names whose `[Content_Types].xml` type is XML (`...xml` or `...+xml`).
fn xml_content_types(parts: &BTreeMap<String, Vec<u8>>) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut exts = BTreeSet::new();
    let mut names = BTreeSet::new();
    let Some(types) = parts.get("[Content_Types].xml").and_then(|b| parse_xml(b)) else {
        return (exts, names);
    };
    for c in &types.children {
        let Node::Elem(e) = c else { continue };
        let get = |n: &str| {
            e.attrs
                .iter()
                .find(|a| a.local == n)
                .map(|a| a.value.clone())
        };
        let is_xml = get("ContentType").is_some_and(|t| t.ends_with("xml"));
        if !is_xml {
            continue;
        }
        if let Some(ext) = get("Extension") {
            exts.insert(ext.to_ascii_lowercase());
        }
        if let Some(name) = get("PartName") {
            names.insert(name.trim_start_matches('/').to_string());
        }
    }
    (exts, names)
}

fn is_xml_part(name: &str, types: &(BTreeSet<String>, BTreeSet<String>)) -> bool {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(ext.as_str(), "xml" | "rels" | "vml")
        || types.0.contains(&ext)
        || types.1.contains(name)
}

/// Whether `saved` holds exactly the parts of `original`, byte for byte (the
/// container itself may differ). `Err` names the first difference.
pub fn parts_identical(original: &[u8], saved: &[u8]) -> Result<(), String> {
    let a = read_parts(original).ok_or("original is not a readable ZIP")?;
    let b = read_parts(saved).ok_or("saved package is not a readable ZIP")?;
    if let Some(name) = a.keys().find(|n| !b.contains_key(*n)) {
        return Err(format!("{name}: missing"));
    }
    if let Some(name) = b.keys().find(|n| !a.contains_key(*n)) {
        return Err(format!("{name}: extra"));
    }
    match a.iter().find(|(n, bytes)| b[*n] != **bytes) {
        Some((name, _)) => Err(format!("{name}: bytes differ")),
        None => Ok(()),
    }
}

/// Compare every part of `saved` with `original`: XML parts canonically,
/// everything else byte for byte, plus missing and extra parts.
pub fn compare_packages(original: &[u8], saved: &[u8]) -> Vec<Finding> {
    let Some(a) = read_parts(original) else {
        return vec![Finding::new(
            "",
            Kind::LoadError,
            String::new(),
            "original is not a readable ZIP".into(),
        )];
    };
    let Some(b) = read_parts(saved) else {
        return vec![Finding::new(
            "",
            Kind::PartBytes,
            String::new(),
            "saved package is not a readable ZIP".into(),
        )];
    };
    let types = xml_content_types(&a);
    let mut out = Vec::new();
    for (name, bytes) in &a {
        let Some(saved_bytes) = b.get(name) else {
            out.push(Finding::new(
                name,
                Kind::PartMissing,
                String::new(),
                String::new(),
            ));
            continue;
        };
        if bytes == saved_bytes {
            continue;
        }
        let trees = is_xml_part(name, &types)
            .then(|| parse_xml(bytes).zip(parse_xml(saved_bytes)))
            .flatten();
        match trees {
            Some((x, y)) => out.extend(compare_xml(name, &x, &y)),
            None => out.push(Finding::new(
                name,
                Kind::PartBytes,
                String::new(),
                format!("{} -> {} bytes", bytes.len(), saved_bytes.len()),
            )),
        }
    }
    for name in b.keys().filter(|n| !a.contains_key(*n)) {
        out.push(Finding::new(
            name,
            Kind::PartExtra,
            String::new(),
            String::new(),
        ));
    }
    out
}

/// Run one file's round trip, turning a panic into a [`Kind::Panic`] finding.
pub fn guarded(f: impl FnOnce() -> Vec<Finding>) -> Vec<Finding> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(findings) => findings,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic".into());
            vec![Finding::new("", Kind::Panic, String::new(), msg)]
        }
    }
}

// ---------------------------------------------------------------------------
// Allowlist

/// A deliberate, benign normalization the gate tolerates.
#[derive(Debug, Clone)]
pub struct AllowRule {
    pub kind: Kind,
    pub part: String,
    pub path: String,
    pub detail: String,
    pub reason: String,
}

/// Parse `kind | part-glob | path-glob | detail-glob | reason` lines. `#`
/// starts a comment. Every rule needs a non-empty reason.
pub fn parse_allowlist(text: &str) -> Result<Vec<AllowRule>, String> {
    let mut rules = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.splitn(5, '|').map(str::trim).collect();
        let [kind, part, path, detail, reason] = fields[..] else {
            return Err(format!(
                "allowlist line {}: expected `kind | part | path | detail | reason`",
                n + 1
            ));
        };
        let kind =
            Kind::parse(kind).ok_or(format!("allowlist line {}: unknown kind {kind:?}", n + 1))?;
        if reason.is_empty() {
            return Err(format!("allowlist line {}: a rule needs a reason", n + 1));
        }
        rules.push(AllowRule {
            kind,
            part: part.to_string(),
            path: path.to_string(),
            detail: detail.to_string(),
            reason: reason.to_string(),
        });
    }
    Ok(rules)
}

impl AllowRule {
    pub fn matches(&self, f: &Finding) -> bool {
        self.kind == f.kind
            && glob(&self.part, &f.part)
            && glob(&self.path, &f.key_path())
            && glob(&self.detail, &f.detail)
    }
}

/// `*` matches any run of characters (including `/`); everything else is literal.
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

// ---------------------------------------------------------------------------
// Baseline

/// One known loss: `file \t part \t kind \t index-free path`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Entry {
    pub file: String,
    pub part: String,
    pub kind: Kind,
    pub path: String,
}

impl Entry {
    pub fn of(file: &str, f: &Finding) -> Entry {
        Entry {
            file: file.to_string(),
            part: f.part.clone(),
            kind: f.kind,
            path: f.key_path(),
        }
    }

    fn line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}",
            self.file,
            self.part,
            self.kind.as_str(),
            self.path
        )
    }
}

pub fn parse_baseline(text: &str) -> Result<BTreeSet<Entry>, String> {
    let mut out = BTreeSet::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let [file, part, kind, path] = fields[..] else {
            return Err(format!(
                "baseline line {}: expected 4 tab-separated fields",
                n + 1
            ));
        };
        let kind =
            Kind::parse(kind).ok_or(format!("baseline line {}: unknown kind {kind:?}", n + 1))?;
        out.insert(Entry {
            file: file.into(),
            part: part.into(),
            kind,
            path: path.into(),
        });
    }
    Ok(out)
}

/// A class of known loss and the issue that owns its fix. Every baseline
/// entry must fall into a class; the gate fails on an unclassified entry.
pub struct LossClass {
    pub name: &'static str,
    pub issue: &'static str,
    pub matches: fn(&Entry) -> bool,
}

/// How the run compares with the baseline, for the files present in the run.
#[derive(Debug, Default)]
pub struct Verdict {
    /// Losses not in the baseline (with an example finding each).
    pub new: Vec<(Entry, Finding)>,
    /// Baseline entries for files in the run that no longer reproduce.
    pub stale: Vec<Entry>,
}

impl Verdict {
    pub fn passed(&self) -> bool {
        self.new.is_empty() && self.stale.is_empty()
    }
}

/// Judge the run's (non-allowlisted) findings against the baseline. Only files
/// in `present` are judged: a baseline entry for a file this run did not see
/// is neither new nor stale.
pub fn judge(
    findings: &[(String, Finding)],
    present: &BTreeSet<String>,
    baseline: &BTreeSet<Entry>,
) -> Verdict {
    let mut seen: BTreeMap<Entry, Finding> = BTreeMap::new();
    for (file, f) in findings {
        seen.entry(Entry::of(file, f)).or_insert_with(|| f.clone());
    }
    let new = seen
        .iter()
        .filter(|(e, _)| !baseline.contains(*e))
        .map(|(e, f)| (e.clone(), f.clone()))
        .collect();
    let stale = baseline
        .iter()
        .filter(|e| present.contains(&e.file) && !seen.contains_key(*e))
        .cloned()
        .collect();
    Verdict { new, stale }
}

/// The baseline after this run: entries for files in `present` are replaced
/// by what the run found; entries for every other file are kept.
pub fn updated_baseline(
    findings: &[(String, Finding)],
    present: &BTreeSet<String>,
    baseline: &BTreeSet<Entry>,
) -> BTreeSet<Entry> {
    baseline
        .iter()
        .filter(|e| !present.contains(&e.file))
        .cloned()
        .chain(findings.iter().map(|(file, f)| Entry::of(file, f)))
        .collect()
}

/// Render the baseline grouped by loss class, each under its issue link.
/// Returns the text and the entries no class claims.
pub fn render_baseline(entries: &BTreeSet<Entry>, classes: &[LossClass]) -> (String, Vec<Entry>) {
    let mut out = String::from(
        "# Round-trip fidelity baseline (#1060): losses known today, one per\n\
         # file / part / kind / index-free path. This file may only shrink: the gate\n\
         # fails on a loss not listed here AND on a listed loss that no longer\n\
         # reproduces. Regenerate with FIDELITY_UPDATE_BASELINE=1 (docs/fidelity-gate.md).\n",
    );
    let mut unclassified = Vec::new();
    let mut groups: Vec<Vec<&Entry>> = classes.iter().map(|_| Vec::new()).collect();
    for e in entries {
        match classes.iter().position(|c| (c.matches)(e)) {
            Some(i) => groups[i].push(e),
            None => unclassified.push(e.clone()),
        }
    }
    for (class, group) in classes.iter().zip(&groups) {
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("\n# {} ({})\n", class.name, class.issue));
        for e in group {
            out.push_str(&e.line());
            out.push('\n');
        }
    }
    if !unclassified.is_empty() {
        out.push_str("\n# UNCLASSIFIED: needs a loss class and an issue\n");
        for e in &unclassified {
            out.push_str(&e.line());
            out.push('\n');
        }
    }
    (out, unclassified)
}
