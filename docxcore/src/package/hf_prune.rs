//! Header and footer parts no section references (#771). Link to Previous,
//! Remove and clearing Different Odd & Even Pages drop a section's
//! `w:headerReference`/`w:footerReference` but keep the part in the package,
//! so an undo can bring the reference back. A save drops those parts from the
//! bytes it writes, with their relationship, content-type override and own
//! `_rels`, as Word does: discarded header text does not stay in the file.

use super::doc_kind::resolve;
use super::{Package, decode_xml_part, part_rels_name, remove_tags_matching, tag_attr};
use crate::load::start_tags;
use crate::serialize::document_to_xml;

const HEADER_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const FOOTER_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";

impl Package {
    /// Remove every header/footer part that `document.xml.rels` names but the
    /// document part a save writes never references, together with its
    /// relationship, its `[Content_Types].xml` override and its own `_rels`
    /// (and that one's override). The removed part names; when there are
    /// none the package is untouched.
    ///
    /// `keeping_document` says which document part the save writes: the
    /// stored one ([`super::save_package_keeping_document`]) or one
    /// serialized from [`Package::document`] ([`super::save_package`]). A
    /// relationship counts as referenced when its id appears as an attribute
    /// value anywhere in that XML, so a section break at any depth (a table
    /// cell, a text box, raw XML the model keeps verbatim) keeps its part.
    /// Keeping a part too many is harmless; dropping a referenced one makes
    /// Word refuse the file. A part some other `.rels` still targets (the
    /// glossary's) stays too, and media an orphan used stay.
    pub fn drop_unreferenced_hf_parts(&mut self, keeping_document: bool) -> Vec<String> {
        let Some(doc_name) = self.parts.get(self.doc_index).map(|p| p.0.clone()) else {
            return Vec::new();
        };
        let Some(rels_name) = part_rels_name(&doc_name) else {
            return Vec::new();
        };
        let Some(rels) = self.part_text(&rels_name) else {
            return Vec::new();
        };
        let dir = doc_name.rsplit_once('/').map_or("", |(d, _)| d);
        // (relationship id, part name) of each header/footer relationship no
        // section's sectPr names: an orphan unless the document names it
        // elsewhere.
        let sect_prs = self.section_sect_prs();
        let named = |text: &str, id: &str| {
            text.contains(&format!("\"{id}\"")) || text.contains(&format!("'{id}'"))
        };
        let mut orphans: Vec<(String, String)> = Vec::new();
        // Every part a relationship that stays targets.
        let mut kept: Vec<String> = Vec::new();
        for (_, tag) in start_tags(&rels, "Relationship") {
            if tag_attr(tag, "TargetMode").as_deref() == Some("External") {
                continue;
            }
            let (Some(id), Some(target)) = (tag_attr(tag, "Id"), tag_attr(tag, "Target")) else {
                continue;
            };
            let part = resolve(dir, &target);
            let hf = matches!(
                tag_attr(tag, "Type").as_deref(),
                Some(HEADER_REL | FOOTER_REL)
            );
            // Only an empty `<Relationship/>` can go: removing a part whose
            // relationship stays would leave it dangling.
            if hf && tag.ends_with("/>") && !sect_prs.iter().any(|s| named(s, &id)) {
                orphans.push((id, part));
            } else {
                kept.push(part);
            }
        }
        if orphans.is_empty() {
            return Vec::new();
        }
        // The whole document part, written only when a candidate is left.
        let document = self.written_document_xml(keeping_document);
        orphans.retain(|(id, part)| {
            let used = named(&document, id);
            if used {
                kept.push(part.clone());
            }
            !used
        });
        let own_rels: Vec<String> = orphans
            .iter()
            .filter_map(|(_, part)| part_rels_name(part))
            .collect();
        // What every other `.rels` file targets, resolved against the folder
        // of the part it belongs to.
        for (name, _) in &self.parts {
            if !name.ends_with(".rels")
                || name.eq_ignore_ascii_case(&rels_name)
                || own_rels.iter().any(|r| r.eq_ignore_ascii_case(name))
            {
                continue;
            }
            let Some(xml) = self.part_text(name) else {
                continue;
            };
            let source_dir = rels_source_dir(name);
            for (_, tag) in start_tags(&xml, "Relationship") {
                if tag_attr(tag, "TargetMode").as_deref() == Some("External") {
                    continue;
                }
                if let Some(target) = tag_attr(tag, "Target") {
                    kept.push(resolve(source_dir, &target));
                }
            }
        }
        orphans.retain(|(_, part)| !kept.iter().any(|k| k.eq_ignore_ascii_case(part)));
        if orphans.is_empty() {
            return Vec::new();
        }
        let ids: Vec<&str> = orphans.iter().map(|(id, _)| id.as_str()).collect();
        let rels = remove_tags_matching(&rels, "Relationship", |tag| {
            tag_attr(tag, "Id").is_some_and(|id| ids.contains(&id.as_str()))
        });
        self.set_part_text(&rels_name, &rels);
        let mut removed: Vec<String> = Vec::new();
        for (_, part) in &orphans {
            if !removed.iter().any(|r| r.eq_ignore_ascii_case(part)) {
                removed.push(part.clone());
            }
        }
        let doomed: Vec<String> = removed
            .iter()
            .flat_map(|part| std::iter::once(part.clone()).chain(part_rels_name(part)))
            .collect();
        if let Some(types) = self.part_text("[Content_Types].xml") {
            let gone: Vec<String> = doomed.iter().map(|d| format!("/{d}")).collect();
            let types = remove_tags_matching(&types, "Override", |tag| {
                tag_attr(tag, "PartName")
                    .is_some_and(|n| gone.iter().any(|g| g.eq_ignore_ascii_case(&n)))
            });
            self.set_part_text("[Content_Types].xml", &types);
        }
        self.retain_parts(|n| !doomed.iter().any(|d| d.eq_ignore_ascii_case(n)));
        removed
    }

    /// The main document part as a save writes it: the stored part when
    /// `keeping_document`, else [`Package::document`] serialized, with the
    /// captured trailing sectPr when the document has none (as
    /// [`super::save_package`] adds it). Its root declarations and prolog
    /// (`w:background`) hold no header reference and are left out.
    fn written_document_xml(&self, keeping_document: bool) -> String {
        if keeping_document {
            return self.document_part().map_or_else(String::new, |bytes| {
                decode_xml_part(bytes).map_or_else(
                    || String::from_utf8_lossy(bytes).into_owned(),
                    |xml| xml.into_owned(),
                )
            });
        }
        let mut xml = document_to_xml(&self.document);
        if self.document.trailing_section_properties().is_none() {
            xml.push_str(&self.sect_pr);
        }
        xml
    }
}

/// The folder of the part a `.rels` file belongs to:
/// `word/glossary/_rels/document.xml.rels` -> `word/glossary`, and the
/// package's own `_rels/.rels` -> the root.
fn rels_source_dir(rels: &str) -> &str {
    let dir = rels.rsplit_once('/').map_or("", |(d, _)| d);
    let dir = dir.strip_suffix("_rels").unwrap_or(dir);
    dir.strip_suffix('/').unwrap_or(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, Document, Paragraph};
    use crate::package::{load_package, new_package, save_package, save_package_keeping_document};

    fn names(pkg: &Package) -> Vec<String> {
        pkg.parts.iter().map(|p| p.0.clone()).collect()
    }

    /// A package with a default header and an even one, the even header
    /// carrying its own `_rels` (with an override) naming a picture. The
    /// part names of the default and even headers.
    fn two_header_pkg() -> (Package, String, String) {
        let mut pkg = new_package(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        let default = pkg.create_hf(true, "default").unwrap();
        let even = pkg.create_hf(true, "even").unwrap();
        let own = part_rels_name(&even).unwrap();
        pkg.parts.push((
            own.clone(),
            br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/even.png"/></Relationships>"#.to_vec(),
        ));
        pkg.parts
            .push(("word/media/even.png".into(), b"PNG".to_vec()));
        let types = pkg.part_text("[Content_Types].xml").unwrap().replace(
            "</Types>",
            &format!("<Override PartName=\"/{own}\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/></Types>"),
        );
        pkg.set_part_text("[Content_Types].xml", &types);
        (pkg, default, even)
    }

    /// Un-reference the even header, as clearing Different Odd & Even does.
    fn unlink_even(pkg: &mut Package) {
        let sect = crate::sect::set_hf_reference(pkg.sect_pr(), true, "even", None);
        pkg.set_sect_pr(sect);
    }

    fn rid_of(pkg: &Package, part: &str) -> String {
        let target = part.strip_prefix("word/").unwrap();
        let rels = pkg.part_text("word/_rels/document.xml.rels").unwrap();
        start_tags(&rels, "Relationship")
            .into_iter()
            .find(|(_, t)| tag_attr(t, "Target").as_deref() == Some(target))
            .and_then(|(_, t)| tag_attr(t, "Id"))
            .unwrap()
    }

    #[test]
    fn drop_unreferenced_hf_parts_removes_orphan_part_rel_override_and_rels() {
        let (mut pkg, default, even) = two_header_pkg();
        unlink_even(&mut pkg);
        let default_bytes = pkg.part(&default).unwrap().to_vec();
        let even_rid = rid_of(&pkg, &even);
        let own = part_rels_name(&even).unwrap();

        assert_eq!(pkg.drop_unreferenced_hf_parts(false), vec![even.clone()]);

        let parts = names(&pkg);
        assert!(!parts.contains(&even), "{parts:?}");
        assert!(!parts.contains(&own), "{parts:?}");
        let rels = pkg.part_text("word/_rels/document.xml.rels").unwrap();
        assert!(!rels.contains(&format!("\"{even_rid}\"")), "{rels}");
        let types = pkg.part_text("[Content_Types].xml").unwrap();
        assert!(!types.contains(&format!("/{even}\"")), "{types}");
        assert!(!types.contains(&format!("/{own}\"")), "{types}");
        // The referenced header stays byte for byte, with its rel and override.
        assert_eq!(pkg.part(&default), Some(default_bytes.as_slice()));
        assert!(rels.contains(&format!("\"{}\"", rid_of(&pkg, &default))));
        assert!(types.contains(&format!("/{default}\"")), "{types}");
        // Media stay (A5), and the result still loads.
        assert!(parts.contains(&"word/media/even.png".to_string()));
        assert!(load_package(&save_package(&pkg)).is_ok());
    }

    /// Two relationships to one part, one still referenced: the part stays,
    /// and so does the referenced relationship.
    #[test]
    fn drop_unreferenced_hf_parts_keeps_referenced_and_shared_parts() {
        let (mut pkg, default, _) = two_header_pkg();
        let rels = pkg.part_text("word/_rels/document.xml.rels").unwrap();
        let target = default.strip_prefix("word/").unwrap();
        let rels = rels.replace(
            "</Relationships>",
            &format!("<Relationship Id=\"rIdSpare\" Type=\"{HEADER_REL}\" Target=\"{target}\"/></Relationships>"),
        );
        pkg.set_part_text("word/_rels/document.xml.rels", &rels);

        assert!(pkg.drop_unreferenced_hf_parts(false).is_empty());
        assert!(names(&pkg).contains(&default));
        let rels = pkg.part_text("word/_rels/document.xml.rels").unwrap();
        assert!(rels.contains("rIdSpare"), "{rels}");
    }

    /// Without an orphan both save branches write exactly today's bytes.
    #[test]
    fn drop_unreferenced_hf_parts_is_noop_without_orphans() {
        let (pkg, _, _) = two_header_pkg();
        let mut pruned = pkg.clone();
        assert!(pruned.drop_unreferenced_hf_parts(false).is_empty());
        assert_eq!(save_package(&pruned), save_package(&pkg));
        assert_eq!(
            save_package_keeping_document(&pruned),
            save_package_keeping_document(&pkg)
        );
    }

    /// A section break inside an unmodelled block keeps its header.
    #[test]
    fn drop_unreferenced_hf_parts_keeps_part_referenced_from_raw_block() {
        let (mut pkg, _, even) = two_header_pkg();
        unlink_even(&mut pkg);
        let rid = rid_of(&pkg, &even);
        pkg.document.body.insert(
            0,
            Block::Raw(format!(
                "<w:customXml w:element=\"x\"><w:p><w:pPr><w:sectPr><w:headerReference w:type=\"even\" r:id=\"{rid}\"/></w:sectPr></w:pPr></w:p></w:customXml>"
            )),
        );
        assert!(pkg.drop_unreferenced_hf_parts(false).is_empty());
        assert!(names(&pkg).contains(&even));
    }

    /// The body parsed from `inner` (body XML), the even header's id
    /// spliced in for `RID`.
    fn body_naming(pkg: &Package, even: &str, inner: &str) -> Vec<Block> {
        let rid = rid_of(pkg, even);
        let xml = format!(
            "<w:document xmlns:w=\"w\" xmlns:r=\"r\" xmlns:v=\"v\"><w:body>{}</w:body></w:document>",
            inner.replace("RID", &rid)
        );
        crate::load::parse_document_xml(&xml, &pkg.document_rels()).body
    }

    /// A section break in a table cell's paragraph keeps its header (FIX r1).
    #[test]
    fn drop_unreferenced_hf_parts_keeps_part_referenced_from_table_cell() {
        let (mut pkg, _, even) = two_header_pkg();
        unlink_even(&mut pkg);
        let mut body = body_naming(
            &pkg,
            &even,
            "<w:tbl><w:tr><w:tc><w:p><w:pPr><w:sectPr><w:headerReference w:type=\"even\" r:id=\"RID\"/></w:sectPr></w:pPr></w:p></w:tc></w:tr></w:tbl>",
        );
        assert!(matches!(body[0], Block::Table(_)), "{body:?}");
        body.extend(pkg.document.body.drain(..));
        pkg.document.body = body;
        assert!(pkg.drop_unreferenced_hf_parts(false).is_empty());
        assert!(names(&pkg).contains(&even));
    }

    /// A section break in a text box's paragraph keeps its header (FIX r1).
    #[test]
    fn drop_unreferenced_hf_parts_keeps_part_referenced_from_text_box() {
        let (mut pkg, _, even) = two_header_pkg();
        unlink_even(&mut pkg);
        let mut body = body_naming(
            &pkg,
            &even,
            "<w:p><w:r><w:pict><v:shape><v:textbox><w:txbxContent><w:p><w:pPr><w:sectPr><w:headerReference w:type=\"even\" r:id=\"RID\"/></w:sectPr></w:pPr></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p>",
        );
        assert!(
            matches!(&body[0], Block::Paragraph(p) if matches!(p.content[0], crate::model::Inline::TextBox { .. })),
            "{body:?}"
        );
        body.extend(pkg.document.body.drain(..));
        pkg.document.body = body;
        assert!(pkg.drop_unreferenced_hf_parts(false).is_empty());
        assert!(names(&pkg).contains(&even));
    }

    /// A save keeping the stored document part reads the references from
    /// that part, not from the model.
    #[test]
    fn drop_unreferenced_hf_parts_reads_the_kept_document_part() {
        let (pkg, _, even) = two_header_pkg();
        let stored = save_package(&pkg);
        let mut pkg = load_package(&stored).unwrap();
        unlink_even(&mut pkg);
        // The stored part still names the even header: kept.
        assert!(pkg.clone().drop_unreferenced_hf_parts(true).is_empty());
        // The document written from the model does not: dropped.
        assert_eq!(pkg.drop_unreferenced_hf_parts(false), vec![even]);
    }

    /// A glossary's relationship (relative to its own folder) keeps the part.
    #[test]
    fn drop_unreferenced_hf_parts_keeps_part_targeted_by_glossary_rels() {
        let (mut pkg, _, even) = two_header_pkg();
        unlink_even(&mut pkg);
        let file = even.strip_prefix("word/").unwrap();
        pkg.parts.push((
            "word/glossary/_rels/document.xml.rels".into(),
            format!(
                "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{HEADER_REL}\" Target=\"../{file}\"/></Relationships>"
            )
            .into_bytes(),
        ));
        assert!(pkg.drop_unreferenced_hf_parts(false).is_empty());
        assert!(names(&pkg).contains(&even));
    }

    /// The orphan's own `_rels` override goes with it; the referenced
    /// header's own `_rels` (and its picture) stay.
    #[test]
    fn drop_unreferenced_hf_parts_drops_rels_override() {
        let (mut pkg, default, even) = two_header_pkg();
        // Give the default header a `_rels` too, then orphan the even one.
        let own_default = part_rels_name(&default).unwrap();
        pkg.parts.push((
            own_default.clone(),
            br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/default.png"/></Relationships>"#.to_vec(),
        ));
        pkg.parts
            .push(("word/media/default.png".into(), b"PNG".to_vec()));
        unlink_even(&mut pkg);
        let own_even = part_rels_name(&even).unwrap();
        let types = pkg.part_text("[Content_Types].xml").unwrap();
        assert!(types.contains(&format!("/{own_even}\"")), "{types}");

        pkg.drop_unreferenced_hf_parts(false);

        let types = pkg.part_text("[Content_Types].xml").unwrap();
        assert!(!types.contains(&format!("/{own_even}\"")), "{types}");
        let parts = names(&pkg);
        assert!(parts.contains(&own_default), "{parts:?}");
        assert!(parts.contains(&"word/media/default.png".to_string()));
    }

    #[test]
    fn rels_source_dirs() {
        assert_eq!(rels_source_dir("word/_rels/document.xml.rels"), "word");
        assert_eq!(
            rels_source_dir("word/glossary/_rels/document.xml.rels"),
            "word/glossary"
        );
        assert_eq!(rels_source_dir("_rels/.rels"), "");
    }
}
