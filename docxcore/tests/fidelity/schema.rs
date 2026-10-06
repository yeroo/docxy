//! Schema validation of saved WordprocessingML (#1083): a dependency-free
//! subset of the ECMA-376 content models of the containers docxy writes, so a
//! save that Word would reject as corrupt fails CI without Word or the Open
//! XML SDK. See "Schema validation" in `docs/fidelity-gate.md`.
//!
//! Each modeled container is a list of slots in schema order. A slot is a set
//! of child names that may occur once or repeatedly. A child is checked by
//! name only (its own content is checked when it is a modeled container too):
//!
//! - `not-allowed`: a `w:` child that no slot of its parent allows;
//! - `order`: a child whose slot comes before one already seen;
//! - `duplicate`: a second child in a slot that allows one;
//! - `missing`: a required slot left empty (named after the slot's first child).
//!
//! Children in another namespace are not checked (extensions, DrawingML,
//! math), but their subtrees are searched for `w:` containers, so text box
//! paragraphs are checked too. `mc:AlternateContent` subtrees are skipped.

use std::collections::BTreeMap;

use super::{Elem, Node, child_steps, parse_xml, read_parts, strip_indices};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const W_STRICT: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";
const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    NotAllowed,
    Order,
    Duplicate,
    Missing,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::NotAllowed => "not-allowed",
            Rule::Order => "order",
            Rule::Duplicate => "duplicate",
            Rule::Missing => "missing",
        }
    }
}

/// One violation in one part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub part: String,
    pub rule: Rule,
    /// The offending child (`w:` local names are written `w:<local>`).
    pub child: String,
    /// The parent's path, with indices: `/w:document/w:body/w:p[3]`.
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

// ---------------------------------------------------------------------------
// Content models (ECMA-376 Part 1, transitional wml.xsd)

#[derive(Clone, Copy)]
enum Occurs {
    Optional,
    Required,
    Many,
    /// One or more.
    Plus,
}

/// The child names a slot takes: one name, or the union of name groups.
#[derive(Clone, Copy)]
enum Names {
    One(&'static str),
    Groups(&'static [&'static [&'static str]]),
}

impl Names {
    fn contains(self, name: &str) -> bool {
        match self {
            Names::One(n) => n == name,
            Names::Groups(gs) => gs.iter().any(|g| g.contains(&name)),
        }
    }

    fn first(self) -> &'static str {
        match self {
            Names::One(n) => n,
            Names::Groups(gs) => gs[0][0],
        }
    }
}

type Slot = (Names, Occurs);

use Names::{Groups, One};
use Occurs::{Many, Optional, Plus, Required};

/// EG_RangeMarkupElements and the rest of EG_RunLevelElements (less math,
/// which is not `w:`).
const RUN_LEVEL: &[&str] = &[
    "bookmarkStart",
    "bookmarkEnd",
    "moveFromRangeStart",
    "moveFromRangeEnd",
    "moveToRangeStart",
    "moveToRangeEnd",
    "commentRangeStart",
    "commentRangeEnd",
    "customXmlInsRangeStart",
    "customXmlInsRangeEnd",
    "customXmlDelRangeStart",
    "customXmlDelRangeEnd",
    "customXmlMoveFromRangeStart",
    "customXmlMoveFromRangeEnd",
    "customXmlMoveToRangeStart",
    "customXmlMoveToRangeEnd",
    "proofErr",
    "permStart",
    "permEnd",
    "ins",
    "del",
    "moveFrom",
    "moveTo",
];

/// EG_PContent: EG_ContentRunContent, fldSimple, hyperlink, subDoc.
const P_CONTENT: &[&str] = &[
    "customXml",
    "smartTag",
    "sdt",
    "dir",
    "bdo",
    "r",
    "fldSimple",
    "hyperlink",
    "subDoc",
];

/// EG_BlockLevelElts, less the run-level markup (listed separately).
const BLOCK: &[&str] = &["p", "tbl", "customXml", "sdt", "altChunk"];

/// EG_RunInnerContent.
const RUN_CONTENT: &[&str] = &[
    "br",
    "t",
    "contentPart",
    "delText",
    "instrText",
    "delInstrText",
    "noBreakHyphen",
    "softHyphen",
    "dayShort",
    "monthShort",
    "yearShort",
    "dayLong",
    "monthLong",
    "yearLong",
    "annotationRef",
    "footnoteRef",
    "endnoteRef",
    "separator",
    "continuationSeparator",
    "sym",
    "pgNum",
    "cr",
    "tab",
    "object",
    "pict",
    "fldChar",
    "ruby",
    "footnoteReference",
    "endnoteReference",
    "commentReference",
    "drawing",
    "ptab",
    "lastRenderedPageBreak",
];

/// CT_PPrBase, in order.
const PPR_BASE: &[&str] = &[
    "pStyle",
    "keepNext",
    "keepLines",
    "pageBreakBefore",
    "framePr",
    "widowControl",
    "numPr",
    "suppressLineNumbers",
    "pBdr",
    "shd",
    "tabs",
    "suppressAutoHyphens",
    "kinsoku",
    "wordWrap",
    "overflowPunct",
    "topLinePunct",
    "autoSpaceDE",
    "autoSpaceDN",
    "bidi",
    "adjustRightInd",
    "snapToGrid",
    "spacing",
    "ind",
    "contextualSpacing",
    "mirrorIndents",
    "suppressOverlap",
    "jc",
    "textDirection",
    "textAlignment",
    "textboxTightWrap",
    "outlineLvl",
    "divId",
    "cnfStyle",
];

/// EG_RPrBase, in order.
const RPR_BASE: &[&str] = &[
    "rStyle",
    "rFonts",
    "b",
    "bCs",
    "i",
    "iCs",
    "caps",
    "smallCaps",
    "strike",
    "dstrike",
    "outline",
    "shadow",
    "emboss",
    "imprint",
    "noProof",
    "snapToGrid",
    "vanish",
    "webHidden",
    "color",
    "spacing",
    "w",
    "kern",
    "position",
    "sz",
    "szCs",
    "highlight",
    "u",
    "effect",
    "bdr",
    "shd",
    "fitText",
    "vertAlign",
    "rtl",
    "cs",
    "em",
    "lang",
    "eastAsianLayout",
    "specVanish",
    "oMath",
];

/// CT_TblPrBase, in order.
const TBLPR_BASE: &[&str] = &[
    "tblStyle",
    "tblpPr",
    "tblOverlap",
    "bidiVisual",
    "tblStyleRowBandSize",
    "tblStyleColBandSize",
    "tblW",
    "jc",
    "tblCellSpacing",
    "tblInd",
    "tblBorders",
    "shd",
    "tblLayout",
    "tblCellMar",
    "tblLook",
    "tblCaption",
    "tblDescription",
];

/// CT_TblPrExBase, in order.
const TBLPREX_BASE: &[&str] = &[
    "tblW",
    "jc",
    "tblCellSpacing",
    "tblInd",
    "tblBorders",
    "shd",
    "tblLayout",
    "tblCellMar",
    "tblLook",
];

/// CT_TcPrBase (through hideMark), in order.
const TCPR_BASE: &[&str] = &[
    "cnfStyle",
    "tcW",
    "gridSpan",
    "hMerge",
    "vMerge",
    "tcBorders",
    "shd",
    "noWrap",
    "tcMar",
    "textDirection",
    "tcFitText",
    "vAlign",
    "hideMark",
];

/// EG_SectPrContents, in order.
const SECTPR_BASE: &[&str] = &[
    "footnotePr",
    "endnotePr",
    "type",
    "pgSz",
    "pgMar",
    "paperSrc",
    "pgBorders",
    "lnNumType",
    "pgNumType",
    "cols",
    "formProt",
    "vAlign",
    "noEndnote",
    "titlePg",
    "textDirection",
    "bidi",
    "rtlGutter",
    "docGrid",
    "printerSettings",
];

/// CT_TrPrBase: an unordered choice.
const TRPR: &[&str] = &[
    "cnfStyle",
    "divId",
    "gridBefore",
    "gridAfter",
    "wBefore",
    "wAfter",
    "cantSplit",
    "trHeight",
    "tblHeader",
    "tblCellSpacing",
    "jc",
    "hidden",
    "ins",
    "del",
    "trPrChange",
];

/// Block content, as in a body or a cell (EG_BlockLevelElts).
const BLOCKS: Names = Groups(&[BLOCK, RUN_LEVEL]);
/// Paragraph content (EG_PContent and run-level markup).
const INLINE: Names = Groups(&[P_CONTENT, RUN_LEVEL]);

/// Each name once, in order.
fn each(names: &'static [&'static str]) -> impl Iterator<Item = Slot> {
    names.iter().map(|n| (One(n), Optional))
}

fn opt(name: &'static str) -> Slot {
    (One(name), Optional)
}

/// The model of a `w:` container, by its local name and its parent's.
fn model(local: &str, parent: &str) -> Option<Vec<Slot>> {
    // Border and margin sides: transitional `left`/`right` or `start`/`end`.
    let sides = |extra: &'static [&'static str]| -> Vec<Slot> {
        [
            opt("top"),
            (Groups(&[&["left", "start"]]), Optional),
            opt("bottom"),
            (Groups(&[&["right", "end"]]), Optional),
        ]
        .into_iter()
        .chain(each(extra))
        .collect()
    };
    Some(match local {
        "body" => vec![(BLOCKS, Many), opt("sectPr")],
        "hdr" | "ftr" | "footnote" | "endnote" | "comment" | "txbxContent" | "docPartBody" => {
            vec![(BLOCKS, Many)]
        }
        "p" => vec![opt("pPr"), (INLINE, Many)],
        "hyperlink" => vec![(INLINE, Many)],
        "smartTag" => vec![opt("smartTagPr"), (INLINE, Many)],
        "r" => vec![opt("rPr"), (Groups(&[RUN_CONTENT]), Many)],
        "tbl" => vec![
            (Groups(&[RUN_LEVEL]), Many),
            (One("tblPr"), Required),
            (One("tblGrid"), Required),
            (Groups(&[&["tr", "customXml", "sdt"], RUN_LEVEL]), Many),
        ],
        "tr" => vec![
            opt("tblPrEx"),
            opt("trPr"),
            (Groups(&[&["tc", "customXml", "sdt"], RUN_LEVEL]), Many),
        ],
        "tc" => vec![opt("tcPr"), (BLOCKS, Plus)],
        // CT_PPr: also the base (pPrChange) and general (styles) forms.
        "pPr" => each(PPR_BASE)
            .chain([opt("rPr"), opt("sectPr"), opt("pPrChange")])
            .collect(),
        // CT_ParaRPr: the paragraph mark's revision marks come first.
        "rPr" if parent == "pPr" => each(&["ins", "del", "moveFrom", "moveTo"])
            .chain(each(RPR_BASE))
            .chain([opt("rPrChange")])
            .collect(),
        "rPr" => each(RPR_BASE).chain([opt("rPrChange")]).collect(),
        "tblPr" => each(TBLPR_BASE).chain([opt("tblPrChange")]).collect(),
        "tblPrEx" => each(TBLPREX_BASE).chain([opt("tblPrExChange")]).collect(),
        "trPr" => vec![(Groups(&[TRPR]), Many)],
        "tcPr" => each(TCPR_BASE)
            .chain([
                opt("headers"),
                (Groups(&[&["cellIns", "cellDel", "cellMerge"]]), Optional),
                opt("tcPrChange"),
            ])
            .collect(),
        "sectPr" => [(Groups(&[&["headerReference", "footerReference"]]), Many)]
            .into_iter()
            .chain(each(SECTPR_BASE))
            .chain([opt("sectPrChange")])
            .collect(),
        "pBdr" => each(&["top", "left", "bottom", "right", "between", "bar"]).collect(),
        "pgBorders" => each(&["top", "left", "bottom", "right"]).collect(),
        "tblBorders" => sides(&["insideH", "insideV"]),
        "tcBorders" => sides(&["insideH", "insideV", "tl2br", "tr2bl"]),
        "tblCellMar" | "tcMar" => sides(&[]),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Validation

fn is_w(e: &Elem) -> bool {
    e.uri == W || e.uri == W_STRICT
}

/// The name a path uses: `w:<local>` for WordprocessingML, else as written,
/// so a prefix other than `w` does not change a violation's key.
fn step_name(e: &Elem) -> String {
    if is_w(e) {
        format!("w:{}", e.local)
    } else {
        e.qname.clone()
    }
}

/// Every violation in one XML part's tree.
pub fn validate(part: &str, root: &Elem) -> Vec<Violation> {
    let mut out = Vec::new();
    let path = format!("/{}", step_name(root));
    walk(part, &path, root, "", &mut out);
    out
}

fn walk(part: &str, path: &str, e: &Elem, parent: &str, out: &mut Vec<Violation>) {
    if e.uri == MC && e.local == "AlternateContent" {
        return;
    }
    if is_w(e) {
        if let Some(slots) = model(&e.local, parent) {
            check(part, path, e, &slots, out);
        }
    }
    let local = if is_w(e) { e.local.as_str() } else { "" };
    for (child, step) in e.children.iter().zip(child_steps(path, &e.children)) {
        if let Node::Elem(c) = child {
            let step = normalize_step(&step, c);
            walk(part, &step, c, local, out);
        }
    }
}

/// `child_steps` writes the qname as written; key on `w:<local>` instead.
fn normalize_step(step: &str, c: &Elem) -> String {
    let name = step_name(c);
    if name == c.qname {
        return step.to_string();
    }
    let (head, tail) = step.rsplit_once('/').unwrap_or(("", step));
    format!("{head}/{}", tail.replacen(&c.qname, &name, 1))
}

fn check(part: &str, path: &str, e: &Elem, slots: &[Slot], out: &mut Vec<Violation>) {
    let mut cur = 0usize;
    let mut used = vec![0usize; slots.len()];
    let mut push = |rule: Rule, child: String| {
        out.push(Violation {
            part: part.to_string(),
            rule,
            child,
            parent: path.to_string(),
        })
    };
    for c in &e.children {
        let Node::Elem(c) = c else { continue };
        if !is_w(c) {
            continue;
        }
        let name = c.local.as_str();
        let fits = |i: usize| slots[i].0.contains(name);
        let open = |i: usize, used: &[usize]| match slots[i].1 {
            Many | Plus => true,
            Optional | Required => used[i] == 0,
        };
        // The first slot at or after the current one that takes this child.
        if let Some(i) = (cur..slots.len()).find(|&i| fits(i) && open(i, &used)) {
            cur = i;
            used[i] += 1;
        } else if (0..slots.len()).any(|i| fits(i) && !open(i, &used)) {
            push(Rule::Duplicate, step_name(c));
        } else if (0..cur).any(fits) {
            push(Rule::Order, step_name(c));
        } else {
            push(Rule::NotAllowed, step_name(c));
        }
    }
    for (i, (names, occurs)) in slots.iter().enumerate() {
        if matches!(occurs, Required | Plus) && used[i] == 0 {
            push(Rule::Missing, format!("w:{}", names.first()));
        }
    }
}

// ---------------------------------------------------------------------------
// Packages

/// Every violation in every part of `pkg` whose root is a `w:` element.
pub fn validate_package(pkg: &[u8]) -> Vec<Violation> {
    let Some(parts) = read_parts(pkg) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, bytes) in &parts {
        if !name.ends_with(".xml") {
            continue;
        }
        if let Some(root) = parse_xml(bytes).filter(is_w) {
            out.extend(validate(name, &root));
        }
    }
    out
}

/// The violations `saved` adds over `original`: for each key, those beyond
/// the original's count (the first ones of `saved` stand for the others).
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
