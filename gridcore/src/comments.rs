//! Cell comments (a.k.a. notes) and threaded comments for `.xlsx`.
//!
//! Reading covers both the legacy `xl/comments*.xml` note format and the
//! modern `xl/threadedComments/*.xml` conversations (resolving author names
//! through `xl/persons/*.xml`). Authoring writes the legacy note format —
//! the one every Excel version renders — with the full OPC wiring it needs:
//! the comments part, a VML drawing for the note box, the worksheet
//! `<legacyDrawing>` hook, content types, and relationships.

use crate::sheet::{cell_name, parse_col};
use crate::xlsx::{
    add_content_type_override, add_rel, parse_rels, resolve_relative, SheetPackage,
};

const SS_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const COMMENTS_CT: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml";
const COMMENTS_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
const VML_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/vmlDrawing";
const VML_CT: &str = "application/vnd.openxmlformats-officedocument.vmlDrawing";

/// One comment anchored to a cell, flattened for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub sheet: usize,
    pub row: u32,
    pub col: u32,
    pub author: String,
    pub text: String,
    /// A modern threaded conversation reply (as opposed to a legacy note).
    pub threaded: bool,
}

// ---------------------------------------------------------------------------
// XML micro-helpers (string scanning, matching the rest of xlsx.rs)
// ---------------------------------------------------------------------------

/// Value of `name="…"` in the opening-tag text `tag`.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let s = tag.find(&key)? + key.len();
    let e = tag[s..].find('"')? + s;
    Some(&tag[s..e])
}

/// Decode the five predefined XML entities.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Concatenated, decoded text of every `<t>…</t>` inside `frag`.
fn collect_t(frag: &str) -> String {
    let mut out = String::new();
    let mut rest = frag;
    while let Some(open) = rest.find("<t") {
        // Skip `<t` that isn't the text element (e.g. `<text>`).
        let after = &rest[open + 2..];
        let gt = match after.find('>') {
            Some(g) => g,
            None => break,
        };
        let is_text_el = after.as_bytes().first() == Some(&b'>')
            || after.as_bytes().first() == Some(&b' ')
            || after[..gt].starts_with('/');
        if !is_text_el {
            rest = &after[gt..];
            continue;
        }
        if after[..gt].ends_with('/') {
            rest = &after[gt + 1..];
            continue;
        }
        let body_start = open + 2 + gt + 1;
        let close = match rest[body_start..].find("</t>") {
            Some(c) => body_start + c,
            None => break,
        };
        out.push_str(&unescape(&rest[body_start..close]));
        rest = &rest[close + 4..];
    }
    out
}

/// Iterate `<tag …>…</tag>` (or self-closing) elements, yielding each element's
/// full text to `f`.
fn for_each_element(xml: &str, tag: &str, mut f: impl FnMut(&str)) {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut rest = xml;
    while let Some(p) = rest.find(&open) {
        let after = &rest[p..];
        // Guard against a prefix match (`<comment` vs `<commentList`).
        let next = after[open.len()..].chars().next();
        if !matches!(next, Some(' ') | Some('>') | Some('/')) {
            rest = &after[open.len()..];
            continue;
        }
        let gt = match after.find('>') {
            Some(g) => g,
            None => break,
        };
        if after[..gt].ends_with('/') {
            f(&after[..gt + 1]);
            rest = &after[gt + 1..];
            continue;
        }
        let end = match after.find(&close) {
            Some(e) => e + close.len(),
            None => break,
        };
        f(&after[..end]);
        rest = &after[end..];
    }
}

fn ref_to_rc(cell: &str) -> Option<(u32, u32)> {
    let cell = cell.trim_start_matches('$');
    let (col, used) = parse_col(cell)?;
    let row: u32 = cell[used..].trim_start_matches('$').parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col))
}

fn split_part(part: &str) -> (&str, &str) {
    match part.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => ("", part),
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

impl SheetPackage {
    fn part_str(&self, name: &str) -> Option<String> {
        self.part(name)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// Every comment in the workbook, ordered by sheet then row/column.
    pub fn comments(&self) -> Vec<Comment> {
        let persons = self.persons_map();
        let mut out = Vec::new();
        for (idx, ws_part) in self.sheet_parts.iter().enumerate() {
            let (dir, file) = split_part(ws_part);
            let rels_name = format!("{dir}/_rels/{file}.rels");
            let Some(rels_xml) = self.part_str(&rels_name) else {
                continue;
            };
            for (_, ty, target) in parse_rels(&rels_xml) {
                let part = resolve_relative(dir, &target);
                if ty.ends_with("/comments") {
                    if let Some(xml) = self.part_str(&part) {
                        parse_legacy(&xml, idx, &mut out);
                    }
                } else if ty.ends_with("/threadedComment") {
                    if let Some(xml) = self.part_str(&part) {
                        parse_threaded(&xml, idx, &persons, &mut out);
                    }
                }
            }
        }
        out.sort_by_key(|c| (c.sheet, c.row, c.col));
        out
    }

    /// Map every `person` id → display name from any `xl/persons/*.xml` part.
    fn persons_map(&self) -> Vec<(String, String)> {
        let mut map = Vec::new();
        for (name, bytes) in &self.parts {
            if !name.contains("persons/") {
                continue;
            }
            let xml = String::from_utf8_lossy(bytes);
            for_each_element(&xml, "person", |el| {
                if let (Some(id), Some(dn)) = (attr(el, "id"), attr(el, "displayName")) {
                    map.push((id.to_string(), unescape(dn)));
                }
            });
        }
        map
    }
}

fn parse_legacy(xml: &str, sheet: usize, out: &mut Vec<Comment>) {
    let mut authors: Vec<String> = Vec::new();
    if let Some(a0) = xml.find("<authors") {
        let a1 = xml[a0..].find("</authors>").map(|i| a0 + i).unwrap_or(a0);
        for_each_element(&xml[a0..a1], "author", |el| {
            let body = el
                .find('>')
                .and_then(|g| el[g + 1..].rfind("</author>").map(|e| &el[g + 1..g + 1 + e]))
                .unwrap_or("");
            authors.push(unescape(body));
        });
    }
    for_each_element(xml, "comment", |el| {
        let Some(cell) = attr(el, "ref").and_then(ref_to_rc) else {
            return;
        };
        let author = attr(el, "authorId")
            .and_then(|s| s.parse::<usize>().ok())
            .and_then(|i| authors.get(i).cloned())
            .unwrap_or_default();
        out.push(Comment {
            sheet,
            row: cell.0,
            col: cell.1,
            author,
            text: collect_t(el),
            threaded: false,
        });
    });
}

fn parse_threaded(xml: &str, sheet: usize, persons: &[(String, String)], out: &mut Vec<Comment>) {
    for_each_element(xml, "threadedComment", |el| {
        let Some(cell) = attr(el, "ref").and_then(ref_to_rc) else {
            return;
        };
        let author = attr(el, "personId")
            .and_then(|id| persons.iter().find(|(k, _)| k == id))
            .map(|(_, n)| n.clone())
            .unwrap_or_default();
        // Threaded bodies live in a single <text>…</text> (no run wrapping).
        let text = el
            .find("<text>")
            .and_then(|s| el[s + 6..].find("</text>").map(|e| &el[s + 6..s + 6 + e]))
            .map(unescape)
            .unwrap_or_default();
        out.push(Comment {
            sheet,
            row: cell.0,
            col: cell.1,
            author,
            text,
            threaded: true,
        });
    });
}

// ---------------------------------------------------------------------------
// Authoring (legacy note format)
// ---------------------------------------------------------------------------

impl SheetPackage {
    /// Add or replace the legacy note on `(row, col)` of `sheet`.
    pub fn set_comment(&mut self, sheet: usize, row: u32, col: u32, author: &str, text: &str) {
        let Some(ws_part) = self.sheet_parts.get(sheet).cloned() else {
            return;
        };
        let mut notes = self.sheet_notes(sheet);
        notes.retain(|n| !(n.row == row && n.col == col));
        notes.push(Note {
            row,
            col,
            author: author.to_string(),
            text: text.to_string(),
        });
        notes.sort_by_key(|n| (n.row, n.col));
        self.write_notes(sheet, &ws_part, &notes);
    }

    /// Remove the note on `(row, col)` of `sheet` (no-op when absent).
    pub fn remove_comment(&mut self, sheet: usize, row: u32, col: u32) {
        let Some(ws_part) = self.sheet_parts.get(sheet).cloned() else {
            return;
        };
        let mut notes = self.sheet_notes(sheet);
        let before = notes.len();
        notes.retain(|n| !(n.row == row && n.col == col));
        if notes.len() == before {
            return;
        }
        self.write_notes(sheet, &ws_part, &notes);
    }

    /// The legacy notes currently on a sheet (ignores threaded comments).
    fn sheet_notes(&self, sheet: usize) -> Vec<Note> {
        self.comments()
            .into_iter()
            .filter(|c| c.sheet == sheet && !c.threaded)
            .map(|c| Note {
                row: c.row,
                col: c.col,
                author: c.author,
                text: c.text,
            })
            .collect()
    }

    /// Resolve (or mint) the comments + VML part names for a sheet.
    fn comment_part_names(&self, ws_part: &str) -> (String, String) {
        let (dir, file) = split_part(ws_part);
        let rels_name = format!("{dir}/_rels/{file}.rels");
        let (mut comments, mut vml) = (None, None);
        if let Some(rels) = self.part_str(&rels_name) {
            for (_, ty, target) in parse_rels(&rels) {
                if ty.ends_with("/comments") {
                    comments = Some(resolve_relative(dir, &target));
                } else if ty.ends_with("/vmlDrawing") {
                    vml = Some(resolve_relative(dir, &target));
                }
            }
        }
        let n = ws_part
            .rsplit_once("sheet")
            .and_then(|(_, s)| s.trim_end_matches(".xml").parse::<u32>().ok())
            .unwrap_or(sheet_fallback(&comments));
        let comments = comments.unwrap_or_else(|| format!("xl/comments{n}.xml"));
        let vml = vml.unwrap_or_else(|| format!("xl/drawings/vmlDrawing{n}.vml"));
        (comments, vml)
    }

    fn write_notes(&mut self, _sheet: usize, ws_part: &str, notes: &[Note]) {
        let (comments_part, vml_part) = self.comment_part_names(ws_part);
        let (dir, file) = split_part(ws_part);
        let rels_name = format!("{dir}/_rels/{file}.rels");

        if notes.is_empty() {
            // Tear the note wiring back down so the file stays valid.
            self.remove_part(&comments_part);
            self.remove_part(&vml_part);
            self.strip_content_type(&format!("/{comments_part}"));
            self.strip_rels(&rels_name, &comments_part, dir);
            self.strip_rels(&rels_name, &vml_part, dir);
            self.strip_legacy_drawing(ws_part);
            return;
        }

        // comments part
        self.set_part(&comments_part, serialize_comments(notes).into_bytes());
        add_content_type_override(&mut self.parts, &format!("/{comments_part}"), COMMENTS_CT);
        let ct_target = rel_target(dir, &comments_part);
        add_rel(&mut self.parts, &rels_name, COMMENTS_REL, &ct_target);

        // VML drawing part
        self.set_part(&vml_part, serialize_vml(notes).into_bytes());
        self.ensure_vml_default();
        let vml_target = rel_target(dir, &vml_part);
        let vml_rid = add_rel(&mut self.parts, &rels_name, VML_REL, &vml_target);
        // add_rel returns "" when the target already existed; find the rId then.
        let vml_rid = if vml_rid.is_empty() {
            self.find_rid(&rels_name, &vml_target)
        } else {
            vml_rid
        };
        self.ensure_legacy_drawing(ws_part, &vml_rid);
    }
}

/// A legacy note during authoring.
struct Note {
    row: u32,
    col: u32,
    author: String,
    text: String,
}

fn sheet_fallback(existing: &Option<String>) -> u32 {
    existing
        .as_ref()
        .and_then(|p| p.rsplit_once("comments"))
        .and_then(|(_, s)| s.trim_end_matches(".xml").parse().ok())
        .unwrap_or(1)
}

/// Path of `part` relative to the worksheet's rels directory.
fn rel_target(ws_dir: &str, part: &str) -> String {
    // ws_dir = "xl/worksheets"; parts live under "xl/…" → "../…".
    let ws_prefix = format!("{ws_dir}/");
    if let Some(rest) = part.strip_prefix("xl/") {
        if ws_dir == "xl" {
            return rest.to_string();
        }
        return format!("../{rest}");
    }
    part.strip_prefix(&ws_prefix).unwrap_or(part).to_string()
}

fn serialize_comments(notes: &[Note]) -> String {
    use crate::xlsx::esc_text;
    let mut authors: Vec<&str> = Vec::new();
    for n in notes {
        if !authors.contains(&n.author.as_str()) {
            authors.push(&n.author);
        }
    }
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    );
    s.push_str(&format!("<comments xmlns=\"{SS_NS}\"><authors>"));
    for a in &authors {
        s.push_str(&format!("<author>{}</author>", esc_text(a)));
    }
    s.push_str("</authors><commentList>");
    for n in notes {
        let aid = authors.iter().position(|a| *a == n.author).unwrap_or(0);
        s.push_str(&format!(
            "<comment ref=\"{}\" authorId=\"{aid}\"><text><r><t xml:space=\"preserve\">{}</t></r></text></comment>",
            cell_name(n.row, n.col),
            esc_text(&n.text),
        ));
    }
    s.push_str("</commentList></comments>");
    s
}

fn serialize_vml(notes: &[Note]) -> String {
    let mut s = String::from(
        "<xml xmlns:v=\"urn:schemas-microsoft-com:vml\" \
         xmlns:o=\"urn:schemas-microsoft-com:office:office\" \
         xmlns:x=\"urn:schemas-microsoft-com:office:excel\">\
         <o:shapelayout v:ext=\"edit\"><o:idmap v:ext=\"edit\" data=\"1\"/></o:shapelayout>\
         <v:shapetype id=\"_x0000_t202\" coordsize=\"21600,21600\" o:spt=\"202\" \
         path=\"m,l,21600r21600,l21600,xe\"><v:stroke joinstyle=\"miter\"/>\
         <v:path gradientshapeok=\"t\" o:connecttype=\"rect\"/></v:shapetype>",
    );
    for (i, n) in notes.iter().enumerate() {
        let id = 1025 + i;
        s.push_str(&format!(
            "<v:shape id=\"_x0000_s{id}\" type=\"#_x0000_t202\" \
             style=\"position:absolute;margin-left:60pt;margin-top:2pt;width:108pt;height:60pt;\
             z-index:{};visibility:hidden\" fillcolor=\"#ffffe1\" o:insetmode=\"auto\">\
             <v:fill color2=\"#ffffe1\"/><v:shadow on=\"t\" color=\"black\" obscured=\"t\"/>\
             <v:path o:connecttype=\"none\"/>\
             <v:textbox style=\"mso-direction-alt:auto\"><div style=\"text-align:left\"></div></v:textbox>\
             <x:ClientData ObjectType=\"Note\"><x:MoveWithCells/><x:SizeWithCells/>\
             <x:Anchor>{}, 15, {}, 2, {}, 15, {}, 4</x:Anchor>\
             <x:AutoFill>False</x:AutoFill><x:Row>{}</x:Row><x:Column>{}</x:Column></x:ClientData></v:shape>",
            i + 1,
            n.col + 1,
            n.row,
            n.col + 3,
            n.row + 4,
            n.row,
            n.col,
        ));
    }
    s.push_str("</xml>");
    s
}

// ---------------------------------------------------------------------------
// OPC wiring helpers on the package
// ---------------------------------------------------------------------------

impl SheetPackage {
    fn ensure_vml_default(&mut self) {
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == "[Content_Types].xml") {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            if xml.contains("Extension=\"vml\"") {
                return;
            }
            let def = format!("<Default Extension=\"vml\" ContentType=\"{VML_CT}\"/>");
            // Defaults conventionally precede Overrides; insert after the opening tag.
            let updated = match xml.find('>') {
                Some(g) => format!("{}{def}{}", &xml[..=g], &xml[g + 1..]),
                None => xml,
            };
            p.1 = updated.into_bytes();
        }
    }

    fn find_rid(&self, rels_name: &str, target: &str) -> String {
        self.part_str(rels_name)
            .and_then(|xml| {
                let key = format!("Target=\"{target}\"");
                let pos = xml.find(&key)?;
                let head = &xml[..pos];
                let id_pos = head.rfind("Id=\"")? + 4;
                let end = head[id_pos..].find('"')? + id_pos;
                Some(head[id_pos..end].to_string())
            })
            .unwrap_or_default()
    }

    /// Insert `<legacyDrawing r:id="…"/>` into the worksheet part in a
    /// schema-valid position (before any trailing extension elements), and
    /// make sure the `r` namespace is declared. Idempotent.
    fn ensure_legacy_drawing(&mut self, ws_part: &str, rid: &str) {
        if rid.is_empty() {
            return;
        }
        let Some(mut xml) = self.part_str(ws_part) else {
            return;
        };
        if xml.contains("<legacyDrawing ") {
            return;
        }
        // Declare xmlns:r on <worksheet …> if absent.
        if !xml.contains("xmlns:r=") {
            if let Some(g) = xml.find("<worksheet") {
                if let Some(rel) = xml[g..].find('>') {
                    let at = g + rel;
                    xml.insert_str(
                        at,
                        " xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"",
                    );
                }
            }
        }
        let tag = format!("<legacyDrawing r:id=\"{rid}\"/>");
        // legacyDrawing must precede these trailing elements if present.
        const AFTER: &[&str] = &[
            "<legacyDrawingHF",
            "<drawingHF",
            "<picture",
            "<oleObjects",
            "<controls",
            "<webPublishItems",
            "<tableParts",
            "<extLst",
        ];
        let insert_at = AFTER
            .iter()
            .filter_map(|t| xml.find(t))
            .min()
            .or_else(|| xml.rfind("</worksheet>"))
            .unwrap_or(xml.len());
        xml.insert_str(insert_at, &tag);
        self.set_part(ws_part, xml.into_bytes());
    }

    fn strip_legacy_drawing(&mut self, ws_part: &str) {
        let Some(mut xml) = self.part_str(ws_part) else {
            return;
        };
        if let Some(s) = xml.find("<legacyDrawing ") {
            if let Some(e) = xml[s..].find("/>") {
                xml.replace_range(s..s + e + 2, "");
                self.set_part(ws_part, xml.into_bytes());
            }
        }
    }

    fn strip_content_type(&mut self, part_name: &str) {
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == "[Content_Types].xml") {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            let key = format!("PartName=\"{part_name}\"");
            if let Some(s) = xml.find(&key) {
                let start = xml[..s].rfind("<Override").unwrap_or(s);
                if let Some(rel) = xml[start..].find("/>") {
                    let mut out = xml.clone();
                    out.replace_range(start..start + rel + 2, "");
                    p.1 = out.into_bytes();
                }
            }
        }
    }

    fn strip_rels(&mut self, rels_name: &str, part: &str, ws_dir: &str) {
        let target = rel_target(ws_dir, part);
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == rels_name) {
            let xml = String::from_utf8_lossy(&p.1).into_owned();
            let key = format!("Target=\"{target}\"");
            if let Some(s) = xml.find(&key) {
                let start = xml[..s].rfind("<Relationship").unwrap_or(s);
                if let Some(rel) = xml[start..].find("/>") {
                    let mut out = xml.clone();
                    out.replace_range(start..start + rel + 2, "");
                    p.1 = out.into_bytes();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xlsx::{load_xlsx, save_xlsx};

    fn blank() -> SheetPackage {
        crate::xlsx::new_xlsx()
    }

    #[test]
    fn attr_and_text_helpers() {
        assert_eq!(attr("<comment ref=\"B2\" authorId=\"1\">", "ref"), Some("B2"));
        assert_eq!(collect_t("<text><r><t>Hello</t></r><r><t> world</t></r></text>"), "Hello world");
        assert_eq!(ref_to_rc("C3"), Some((2, 2)));
    }

    #[test]
    fn round_trips_a_note() {
        let mut pkg = blank();
        pkg.set_comment(0, 1, 2, "Reviewer", "Check this value");
        let bytes = save_xlsx(&pkg);
        let reloaded = load_xlsx(&bytes).expect("reload");
        let cs = reloaded.comments();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].row, 1);
        assert_eq!(cs[0].col, 2);
        assert_eq!(cs[0].author, "Reviewer");
        assert_eq!(cs[0].text, "Check this value");
        assert!(!cs[0].threaded);
        // The worksheet gained a legacyDrawing hook.
        let ws = reloaded.part("xl/worksheets/sheet1.xml").unwrap();
        assert!(String::from_utf8_lossy(ws).contains("<legacyDrawing "));
    }

    #[test]
    fn replace_and_delete() {
        let mut pkg = blank();
        pkg.set_comment(0, 0, 0, "A", "first");
        pkg.set_comment(0, 0, 0, "B", "second"); // replace same cell
        pkg.set_comment(0, 5, 5, "C", "elsewhere");
        let cs = pkg.comments();
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].text, "second");
        assert_eq!(cs[0].author, "B");

        pkg.remove_comment(0, 0, 0);
        let cs = pkg.comments();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].text, "elsewhere");

        // Removing the last note tears the wiring back down and still saves.
        pkg.remove_comment(0, 5, 5);
        assert!(pkg.comments().is_empty());
        let bytes = save_xlsx(&pkg);
        let reloaded = load_xlsx(&bytes).expect("reload after teardown");
        assert!(reloaded.comments().is_empty());
    }

    #[test]
    fn escapes_special_characters() {
        let mut pkg = blank();
        pkg.set_comment(0, 0, 0, "A & B", "x < y & \"z\"");
        let bytes = save_xlsx(&pkg);
        let cs = load_xlsx(&bytes).unwrap().comments();
        assert_eq!(cs[0].author, "A & B");
        assert_eq!(cs[0].text, "x < y & \"z\"");
    }

    #[test]
    fn reads_threaded_comments() {
        let mut pkg = blank();
        pkg.set_part(
            "xl/persons/person1.xml",
            b"<personList xmlns=\"x\"><person displayName=\"Jane Doe\" id=\"{ABC}\"/></personList>".to_vec(),
        );
        pkg.set_part(
            "xl/threadedComments/threadedComment1.xml",
            b"<ThreadedComments xmlns=\"x\"><threadedComment ref=\"A1\" personId=\"{ABC}\" id=\"{1}\"><text>Looks good</text></threadedComment></ThreadedComments>".to_vec(),
        );
        // Wire the worksheet rels to the threaded part.
        pkg.set_part(
            "xl/worksheets/_rels/sheet1.xml.rels",
            b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.microsoft.com/office/2017/10/relationships/threadedComment\" Target=\"../threadedComments/threadedComment1.xml\"/></Relationships>".to_vec(),
        );
        let cs = pkg.comments();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].author, "Jane Doe");
        assert_eq!(cs[0].text, "Looks good");
        assert!(cs[0].threaded);
    }
}
