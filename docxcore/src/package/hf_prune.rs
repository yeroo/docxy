//! Header and footer parts no section references (#771). Link to Previous,
//! Remove and clearing Different Odd & Even Pages drop a section's
//! `w:headerReference`/`w:footerReference` but keep the part in the package,
//! so an undo can bring the reference back. A save drops those parts from the
//! bytes it writes, with their relationship, content-type override and own
//! `_rels`, as Word does: discarded header text does not stay in the file.

use super::doc_kind::resolve;
use super::{Package, part_rels_name, remove_tags_matching, tag_attr};
use crate::load::start_tags;
use crate::model::Block;

const HEADER_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const FOOTER_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";

impl Package {
    /// Remove every header/footer part that `document.xml.rels` names but no
    /// section of [`Package::document`] references, together with its
    /// relationship, its `[Content_Types].xml` override and its own `_rels`
    /// (and that one's override). The removed part names; when there are
    /// none the package is untouched.
    ///
    /// A relationship counts as referenced when its id appears as an
    /// attribute value in any section's `w:sectPr`, in any section's tracked
    /// change, or anywhere in raw block XML the model keeps verbatim (a
    /// paragraph inside an unmodelled block can end a section). Keeping a
    /// part too many is harmless; dropping a referenced one makes Word
    /// refuse the file. A part some other `.rels` still targets (the
    /// glossary's) stays too, and media an orphan used stay.
    pub fn drop_unreferenced_hf_parts(&mut self) -> Vec<String> {
        let Some(doc_name) = self.parts.get(self.doc_index).map(|p| p.0.clone()) else {
            return Vec::new();
        };
        let Some(rels_name) = part_rels_name(&doc_name) else {
            return Vec::new();
        };
        let Some(rels) = self.part_text(&rels_name) else {
            return Vec::new();
        };
        let texts = self.section_reference_texts();
        let referenced = |id: &str| {
            let (dq, sq) = (format!("\"{id}\""), format!("'{id}'"));
            texts.iter().any(|t| t.contains(&dq) || t.contains(&sq))
        };
        let dir = doc_name.rsplit_once('/').map_or("", |(d, _)| d);
        // (relationship id, part name) of each unreferenced header/footer.
        let mut orphans: Vec<(String, String)> = Vec::new();
        // Every part another document relationship still targets.
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
            if hf && tag.ends_with("/>") && !referenced(&id) {
                orphans.push((id, part));
            } else {
                kept.push(part);
            }
        }
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

    /// The XML a header/footer reference of the document can sit in: every
    /// section's sectPr, the sections' tracked changes, and every raw block,
    /// table cells included.
    fn section_reference_texts(&self) -> Vec<String> {
        fn walk(blocks: &[Block], out: &mut Vec<String>) {
            for block in blocks {
                match block {
                    Block::Paragraph(p) => {
                        if let Some(change) = &p.props.section_property_change {
                            out.push(change.raw.clone());
                        }
                    }
                    Block::Table(t) => {
                        for row in &t.rows {
                            out.extend(row.raw_props.iter().cloned());
                            for cell in &row.cells {
                                walk(&cell.blocks, out);
                            }
                        }
                    }
                    Block::SectionProperties(s) => {
                        out.push(s.raw.clone());
                        if let Some(change) = &s.property_change {
                            out.push(change.raw.clone());
                        }
                    }
                    Block::Raw(raw) => out.push(raw.clone()),
                }
            }
        }
        let mut out: Vec<String> = self
            .section_sect_prs()
            .into_iter()
            .map(str::to_string)
            .collect();
        walk(&self.document.body, &mut out);
        out
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
    use crate::model::{Document, Paragraph};
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

        assert_eq!(pkg.drop_unreferenced_hf_parts(), vec![even.clone()]);

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

        assert!(pkg.drop_unreferenced_hf_parts().is_empty());
        assert!(names(&pkg).contains(&default));
        let rels = pkg.part_text("word/_rels/document.xml.rels").unwrap();
        assert!(rels.contains("rIdSpare"), "{rels}");
    }

    /// Without an orphan both save branches write exactly today's bytes.
    #[test]
    fn drop_unreferenced_hf_parts_is_noop_without_orphans() {
        let (pkg, _, _) = two_header_pkg();
        let mut pruned = pkg.clone();
        assert!(pruned.drop_unreferenced_hf_parts().is_empty());
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
        assert!(pkg.drop_unreferenced_hf_parts().is_empty());
        assert!(names(&pkg).contains(&even));
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
        assert!(pkg.drop_unreferenced_hf_parts().is_empty());
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

        pkg.drop_unreferenced_hf_parts();

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
