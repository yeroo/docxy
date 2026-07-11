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
    SheetPackage, add_content_type_override, add_rel, add_workbook_rel, parse_rels,
    resolve_relative,
};

const SS_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const COMMENTS_CT: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml";
const COMMENTS_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
const VML_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/vmlDrawing";
const VML_CT: &str = "application/vnd.openxmlformats-officedocument.vmlDrawing";

// Modern threaded comments (Excel 2019+ / 365).
const TC_NS: &str = "http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments";
const TC_CT: &str = "application/vnd.ms-excel.threadedcomments+xml";
const TC_REL: &str = "http://schemas.microsoft.com/office/2017/10/relationships/threadedComment";
const PERSON_CT: &str = "application/vnd.ms-excel.person+xml";
const PERSON_REL: &str = "http://schemas.microsoft.com/office/2017/10/relationships/person";

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
                .and_then(|g| {
                    el[g + 1..]
                        .rfind("</author>")
                        .map(|e| &el[g + 1..g + 1 + e])
                })
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
        // `tc=…` authored comments are the down-level *shadow* of a threaded
        // comment — the threaded part is authoritative, so skip them here.
        if author.starts_with("tc=") {
            return;
        }
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

    /// Remove the comment on `(row, col)` of `sheet` — the threaded
    /// conversation if present, otherwise the legacy note.
    pub fn remove_comment(&mut self, sheet: usize, row: u32, col: u32) {
        let Some(ws_part) = self.sheet_parts.get(sheet).cloned() else {
            return;
        };
        // Drop any threaded conversation on the cell first (so the rebuilt
        // legacy shadow no longer includes it).
        let removed_thread = self.remove_thread_entries(&ws_part, row, col);
        let mut notes = self.sheet_notes(sheet);
        let before = notes.len();
        notes.retain(|n| !(n.row == row && n.col == col));
        if notes.len() == before && !removed_thread {
            return;
        }
        self.write_notes(sheet, &ws_part, &notes);
    }

    /// The legacy notes to serialize for a sheet: real notes plus a down-level
    /// *shadow* note for every threaded conversation, so old readers still see
    /// something and modern Excel de-dupes the two via the `tc=` author marker.
    fn sheet_notes(&self, sheet: usize) -> Vec<Note> {
        let all = self.comments();
        let mut notes: Vec<Note> = all
            .iter()
            .filter(|c| c.sheet == sheet && !c.threaded)
            .map(|c| Note {
                row: c.row,
                col: c.col,
                author: c.author.clone(),
                text: c.text.clone(),
            })
            .collect();
        // One shadow per threaded conversation (grouped by cell, in order).
        let mut seen: Vec<(u32, u32)> = Vec::new();
        for c in all.iter().filter(|c| c.sheet == sheet && c.threaded) {
            if seen.contains(&(c.row, c.col)) {
                continue;
            }
            seen.push((c.row, c.col));
            let thread: Vec<&Comment> = all
                .iter()
                .filter(|t| t.sheet == sheet && t.threaded && t.row == c.row && t.col == c.col)
                .collect();
            let pid = guid_from(&[&thread[0].author]);
            let text = thread
                .iter()
                .map(|t| format!("{}:\n    {}", t.author, t.text))
                .collect::<Vec<_>>()
                .join("\n");
            notes.push(Note {
                row: c.row,
                col: c.col,
                author: format!("tc={pid}"),
                text,
            });
        }
        notes.sort_by_key(|n| (n.row, n.col));
        notes
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

// ---------------------------------------------------------------------------
// Authoring (modern threaded conversations)
// ---------------------------------------------------------------------------

impl SheetPackage {
    /// Append a threaded comment on `(row, col)` — a new conversation, or a
    /// reply if the cell already has one. `when` is an ISO-8601 timestamp
    /// (e.g. `2024-01-02T03:04:05Z`); the caller supplies it so the engine
    /// stays clock-free. Writes the persons + threadedComments parts, wires
    /// them, and keeps a legacy shadow note so every reader shows the thread.
    pub fn add_threaded_comment(
        &mut self,
        sheet: usize,
        row: u32,
        col: u32,
        author: &str,
        text: &str,
        when: &str,
    ) {
        use crate::xlsx::{esc_attr, esc_text};
        let Some(ws_part) = self.sheet_parts.get(sheet).cloned() else {
            return;
        };
        let (dir, file) = split_part(&ws_part);
        let rels_name = format!("{dir}/_rels/{file}.rels");

        // Resolve (or mint) the threadedComments and persons part names.
        let tc_part = self
            .find_rel_target(&rels_name, "/threadedComment")
            .unwrap_or_else(|| self.unique_part("xl/threadedComments/threadedComment", ".xml"));
        let persons_part = self
            .parts
            .iter()
            .map(|(n, _)| n.clone())
            .find(|n| n.contains("persons/"))
            .unwrap_or_else(|| "xl/persons/person1.xml".to_string());

        // A stable person id per author name.
        let pid = guid_from(&[author]);
        self.ensure_person(&persons_part, &pid, author);

        // Reply if a root already exists on this cell.
        let cellref = cell_name(row, col);
        let existing = self.part_str(&tc_part).unwrap_or_default();
        let root_id = find_root_id(&existing, &cellref);
        let new_id = guid_from(&[author, &cellref, text, &existing.len().to_string()]);
        let parent = root_id
            .map(|r| format!(" parentId=\"{r}\""))
            .unwrap_or_default();
        let el = format!(
            "<threadedComment ref=\"{cellref}\" dT=\"{}\" personId=\"{pid}\" id=\"{new_id}\"{parent}><text>{}</text></threadedComment>",
            esc_attr(when),
            esc_text(text),
        );
        let updated = if existing.trim().is_empty() {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<ThreadedComments xmlns=\"{TC_NS}\">{el}</ThreadedComments>"
            )
        } else {
            existing.replacen(
                "</ThreadedComments>",
                &format!("{el}</ThreadedComments>"),
                1,
            )
        };
        self.set_part(&tc_part, updated.into_bytes());

        // Content types + relationships (threadedComments on the worksheet,
        // persons on the workbook).
        add_content_type_override(&mut self.parts, &format!("/{tc_part}"), TC_CT);
        add_content_type_override(&mut self.parts, &format!("/{persons_part}"), PERSON_CT);
        add_rel(
            &mut self.parts,
            &rels_name,
            TC_REL,
            &rel_target(dir, &tc_part),
        );
        let persons_wb_target = persons_part.strip_prefix("xl/").unwrap_or(&persons_part);
        add_workbook_rel(&mut self.parts, PERSON_REL, persons_wb_target);

        // Rebuild the legacy shadow so down-level readers see the thread too.
        let notes = self.sheet_notes(sheet);
        self.write_notes(sheet, &ws_part, &notes);
    }

    /// Ensure `persons_part` declares person `pid` with `display`.
    fn ensure_person(&mut self, persons_part: &str, pid: &str, display: &str) {
        use crate::xlsx::esc_attr;
        let el = format!(
            "<person displayName=\"{}\" id=\"{pid}\" providerId=\"None\"/>",
            esc_attr(display)
        );
        match self.part_str(persons_part) {
            Some(xml) if xml.contains(pid) => {}
            Some(xml) => {
                let u = xml.replacen("</personList>", &format!("{el}</personList>"), 1);
                self.set_part(persons_part, u.into_bytes());
            }
            None => {
                let body = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<personList xmlns=\"{TC_NS}\">{el}</personList>"
                );
                self.set_part(persons_part, body.into_bytes());
            }
        }
    }

    /// Remove every threaded comment on `(row, col)` from the sheet's thread
    /// part; returns whether anything was removed.
    fn remove_thread_entries(&mut self, ws_part: &str, row: u32, col: u32) -> bool {
        let (dir, file) = split_part(ws_part);
        let rels_name = format!("{dir}/_rels/{file}.rels");
        let Some(tc_part) = self.find_rel_target(&rels_name, "/threadedComment") else {
            return false;
        };
        let Some(xml) = self.part_str(&tc_part) else {
            return false;
        };
        let cellref = cell_name(row, col);
        let mut out = String::new();
        let mut removed = false;
        let mut rest = xml.as_str();
        while let Some(p) = rest.find("<threadedComment") {
            out.push_str(&rest[..p]);
            let after = &rest[p..];
            let end = match after.find("</threadedComment>") {
                Some(e) => e + "</threadedComment>".len(),
                None => {
                    // self-closing?
                    after.find("/>").map(|e| e + 2).unwrap_or(after.len())
                }
            };
            let el = &after[..end];
            if attr(el, "ref") == Some(cellref.as_str()) {
                removed = true;
            } else {
                out.push_str(el);
            }
            rest = &after[end..];
        }
        out.push_str(rest);
        if removed {
            // If the thread list is now empty, tear its wiring down too.
            if !out.contains("<threadedComment") {
                self.remove_part(&tc_part);
                self.strip_content_type(&format!("/{tc_part}"));
                self.strip_rels(&rels_name, &tc_part, dir);
            } else {
                self.set_part(&tc_part, out.into_bytes());
            }
        }
        removed
    }

    /// The target part of the first relationship whose type ends with `suffix`.
    fn find_rel_target(&self, rels_name: &str, suffix: &str) -> Option<String> {
        let dir = rels_name
            .split_once("/_rels/")
            .map(|(d, _)| d)
            .unwrap_or("");
        let rels = self.part_str(rels_name)?;
        parse_rels(&rels)
            .into_iter()
            .find(|(_, ty, _)| ty.ends_with(suffix))
            .map(|(_, _, target)| resolve_relative(dir, &target))
    }

    /// A `{prefix}{n}{ext}` part name not already present.
    fn unique_part(&self, prefix: &str, ext: &str) -> String {
        let mut n = 1;
        while self.part(&format!("{prefix}{n}{ext}")).is_some() {
            n += 1;
        }
        format!("{prefix}{n}{ext}")
    }
}

/// The `id` of the root (parent-less) threaded comment on `cellref`, if any.
fn find_root_id(xml: &str, cellref: &str) -> Option<String> {
    let mut found = None;
    for_each_element(xml, "threadedComment", |el| {
        if found.is_none() && attr(el, "ref") == Some(cellref) && attr(el, "parentId").is_none() {
            found = attr(el, "id").map(str::to_string);
        }
    });
    found
}

/// FNV-1a 64-bit hash.
fn hash64(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// A deterministic, stable GUID string from the joined parts (so the same
/// author always maps to the same person id, and threads stay linkable).
fn guid_from(parts: &[&str]) -> String {
    let joined = parts.join("|");
    let a = hash64(&joined);
    let b = hash64(&format!("{joined}#salt"));
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        a as u16,
        (b >> 48) as u16,
        b & 0xFFFF_FFFF_FFFF,
    )
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
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
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
        if let Some(p) = self
            .parts
            .iter_mut()
            .find(|(n, _)| n == "[Content_Types].xml")
        {
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
        if let Some(p) = self
            .parts
            .iter_mut()
            .find(|(n, _)| n == "[Content_Types].xml")
        {
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
        assert_eq!(
            attr("<comment ref=\"B2\" authorId=\"1\">", "ref"),
            Some("B2")
        );
        assert_eq!(
            collect_t("<text><r><t>Hello</t></r><r><t> world</t></r></text>"),
            "Hello world"
        );
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
            b"<personList xmlns=\"x\"><person displayName=\"Jane Doe\" id=\"{ABC}\"/></personList>"
                .to_vec(),
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

    #[test]
    fn authors_a_threaded_conversation() {
        let mut pkg = blank();
        pkg.add_threaded_comment(0, 1, 1, "Ana", "Is this final?", "2024-01-02T03:04:05Z");
        pkg.add_threaded_comment(0, 1, 1, "Bob", "Yes, ship it", "2024-01-02T04:00:00Z");
        // A second, independent thread elsewhere.
        pkg.add_threaded_comment(0, 5, 0, "Ana", "Typo here", "2024-01-02T05:00:00Z");

        let bytes = save_xlsx(&pkg);
        let reloaded = load_xlsx(&bytes).expect("reload");
        let cs = reloaded.comments();
        // Three threaded comments, no duplicate legacy shadows leaking through.
        assert_eq!(cs.iter().filter(|c| c.threaded).count(), 3);
        assert!(cs.iter().all(|c| c.threaded));
        assert!(!cs.iter().any(|c| c.author.starts_with("tc=")));

        // The reply is linked under the same cell's root.
        let a1: Vec<&Comment> = cs.iter().filter(|c| c.row == 1 && c.col == 1).collect();
        assert_eq!(a1.len(), 2);
        assert_eq!(a1[0].author, "Ana");
        assert_eq!(a1[1].author, "Bob");
        assert_eq!(a1[1].text, "Yes, ship it");

        // The threaded parts and their wiring exist.
        assert!(reloaded.part("xl/persons/person1.xml").is_some());
        assert!(
            reloaded
                .part_names()
                .iter()
                .any(|n| n.contains("threadedComments/"))
        );
        // A legacy shadow rides along for down-level readers.
        assert!(
            reloaded
                .part_names()
                .iter()
                .any(|n| n.starts_with("xl/comments"))
        );
    }

    #[test]
    fn deleting_a_thread_removes_it_cleanly() {
        let mut pkg = blank();
        pkg.add_threaded_comment(0, 0, 0, "Ana", "First", "2024-01-01T00:00:00Z");
        pkg.add_threaded_comment(0, 2, 2, "Bob", "Second", "2024-01-01T00:00:00Z");
        assert_eq!(pkg.comments().len(), 2);

        pkg.remove_comment(0, 0, 0);
        let cs = pkg.comments();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].row, 2);

        // Round-trips with just the survivor.
        let reloaded = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let cs = reloaded.comments();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].text, "Second");
        assert!(cs[0].threaded);
    }

    #[test]
    fn legacy_note_and_thread_coexist_on_a_sheet() {
        let mut pkg = blank();
        pkg.set_comment(0, 0, 0, "Reviewer", "A plain note");
        pkg.add_threaded_comment(0, 3, 3, "Ana", "A conversation", "2024-01-01T00:00:00Z");
        let reloaded = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let cs = reloaded.comments();
        assert_eq!(cs.len(), 2);
        let note = cs.iter().find(|c| !c.threaded).unwrap();
        let thread = cs.iter().find(|c| c.threaded).unwrap();
        assert_eq!(note.text, "A plain note");
        assert_eq!(thread.text, "A conversation");
    }
}
