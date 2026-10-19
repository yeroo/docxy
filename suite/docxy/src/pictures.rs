//! Inline pictures in the document view (#1432): the media bytes behind a
//! `w:drawing`'s `r:embed`, read from the package through the story part's
//! own relationships and handed to gpui to decode. A rid means a different
//! part in `document.xml.rels` and in a header's, so every lookup is keyed by
//! the story part as well.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use docxcore::package::Package;
use gpui::{Image, ImageFormat};

/// The media behind `rid` in `part`'s relationships: the extension
/// [`docxcore::image::sniff`] found and the bytes. `None` when the rid, its
/// target part (an external one is none) or a recognisable image is missing.
pub fn resolve(pkg: &Package, part: &str, rid: &str) -> Option<(&'static str, Vec<u8>)> {
    let bytes = pkg.part(&pkg.part_rel_target(part, rid)?)?;
    let kind = docxcore::image::sniff(bytes)?;
    Some((kind.ext, bytes.to_vec()))
}

fn format_of(ext: &str) -> Option<ImageFormat> {
    match ext {
        "png" => Some(ImageFormat::Png),
        "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "bmp" => Some(ImageFormat::Bmp),
        _ => None,
    }
}

/// A picture's story part and relationship id.
type PictureKey = (String, String);

/// What a tab has read of its package's pictures, kept across frames so a
/// frame resolves, copies and hashes each picture once. It is valid for one
/// [`Package::parts_stamp`]: any change to the package's parts empties it.
#[derive(Default)]
pub struct Cache {
    stamp: u64,
    images: HashMap<PictureKey, Option<Arc<Image>>>,
}

/// The pictures one frame draws, through the tab's [`Cache`].
pub struct Pictures<'a> {
    pkg: &'a Package,
    cache: &'a RefCell<Cache>,
}

impl<'a> Pictures<'a> {
    pub fn new(pkg: &'a Package, cache: &'a RefCell<Cache>) -> Self {
        let stamp = pkg.parts_stamp();
        let mut held = cache.borrow_mut();
        if held.stamp != stamp {
            held.images.clear();
            held.stamp = stamp;
        }
        drop(held);
        Pictures { pkg, cache }
    }

    /// The image of an inline picture run `raw` in `part`, as gpui will
    /// decode it; `None` keeps the frame. Only a miss reads the package.
    pub fn image(&self, part: &str, raw: &str) -> Option<Arc<Image>> {
        let rid = docxcore::image::inline_embed(raw)?;
        let key = (part.to_string(), rid);
        if let Some(hit) = self.cache.borrow().images.get(&key) {
            return hit.clone();
        }
        let found = resolve(self.pkg, part, &key.1)
            .and_then(|(ext, bytes)| Some(Arc::new(Image::from_bytes(format_of(ext)?, bytes))));
        self.cache.borrow_mut().images.insert(key, found.clone());
        found
    }
}

/// A story's pictures: the frame's cache and the part the story came from.
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    pub pictures: &'a Pictures<'a>,
    pub part: &'a str,
}

/// The scope of `part` in `pictures`; `None` when either is absent.
pub fn scope<'a>(pictures: Option<&'a Pictures<'a>>, part: Option<&'a str>) -> Option<Scope<'a>> {
    Some(Scope {
        pictures: pictures?,
        part: part?,
    })
}

impl Scope<'_> {
    pub fn image(&self, raw: &str) -> Option<Arc<Image>> {
        self.pictures.image(self.part, raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::model::Document;
    use docxcore::package::new_package;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x02\0\0\0\x03\x08\x02\0\0\0";

    fn png(tag: u8) -> Vec<u8> {
        [PNG, &[tag]].concat()
    }

    fn add(pkg: &mut docxcore::package::Package, part: &str, bytes: &[u8]) -> String {
        let kind = docxcore::image::sniff(bytes).expect("a png");
        pkg.add_part_image(part, bytes, &kind).expect("added")
    }

    /// A body picture and a header picture; each story's rid names its own.
    fn package() -> (docxcore::package::Package, String, String, String) {
        let mut pkg = new_package(Document { body: vec![] });
        let (_, header) = pkg.create_hf_part(true, "<w:p/>").unwrap();
        let body_rid = add(&mut pkg, "word/document.xml", &png(1));
        let head_rid = add(&mut pkg, &header, &png(2));
        (pkg, header, body_rid, head_rid)
    }

    #[test]
    fn a_rid_resolves_through_its_own_story_part() {
        let (pkg, header, body_rid, head_rid) = package();
        assert_eq!(
            resolve(&pkg, "word/document.xml", &body_rid),
            Some(("png", png(1)))
        );
        assert_eq!(resolve(&pkg, &header, &head_rid), Some(("png", png(2))));
        // The header's rid is not the body's: the body has no such relationship
        // (or a different one), never the header's picture.
        assert_ne!(
            resolve(&pkg, "word/document.xml", &head_rid),
            Some(("png", png(2)))
        );
    }

    #[test]
    fn the_same_rid_in_two_parts_is_two_pictures() {
        let mut pkg = new_package(Document { body: vec![] });
        let (_, h1) = pkg.create_hf_part(true, "<w:p/>").unwrap();
        let (_, h2) = pkg.create_hf_part(false, "<w:p/>").unwrap();
        let (a, b) = (add(&mut pkg, &h1, &png(1)), add(&mut pkg, &h2, &png(2)));
        assert_eq!(a, b, "both stories number from rId1");
        assert_eq!(resolve(&pkg, &h1, &a), Some(("png", png(1))));
        assert_eq!(resolve(&pkg, &h2, &b), Some(("png", png(2))));
    }

    #[test]
    fn an_absolute_target_is_from_the_package_root() {
        let (mut pkg, header, _, rid) = package();
        let rels = pkg.part_text("word/_rels/header1.xml.rels").unwrap();
        let rels = rels.replace("media/image", "/word/media/image");
        assert!(pkg.set_part("word/_rels/header1.xml.rels", rels.into_bytes()));
        assert_eq!(resolve(&pkg, &header, &rid), Some(("png", png(2))));
    }

    #[test]
    fn a_missing_rid_part_or_unrecognised_bytes_resolve_to_none() {
        let (mut pkg, header, _, rid) = package();
        assert_eq!(resolve(&pkg, &header, "rId99"), None);
        assert_eq!(resolve(&pkg, "word/footer9.xml", &rid), None);
        // The media part holds something that is not an image.
        assert!(pkg.set_part("word/media/image2.png", b"not an image".to_vec()));
        assert_eq!(resolve(&pkg, &header, &rid), None);
        // The media part is gone.
        let rels = pkg.part_text("word/_rels/header1.xml.rels").unwrap();
        let rels = rels.replace("media/image2.png", "media/gone.png");
        assert!(pkg.set_part("word/_rels/header1.xml.rels", rels.into_bytes()));
        assert_eq!(resolve(&pkg, &header, &rid), None);
    }

    #[test]
    fn only_an_inline_picture_run_gives_an_image_and_it_is_cached() {
        let (pkg, header, body_rid, _) = package();
        let cache = RefCell::default();
        let pics = Pictures::new(&pkg, &cache);
        let run = docxcore::image::inline_picture_run(&body_rid, 9525, 9525, 1, "a.png");
        let (a, b) = (
            pics.image("word/document.xml", &run).unwrap(),
            pics.image("word/document.xml", &run).unwrap(),
        );
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.format, ImageFormat::Png);
        assert_eq!(a.bytes, png(1));
        assert!(pics.image(&header, "<w:r><w:ptab/></w:r>").is_none());
        let anchored = run
            .replace("<wp:inline", "<wp:anchor")
            .replace("</wp:inline>", "</wp:anchor>");
        assert!(pics.image("word/document.xml", &anchored).is_none());
    }

    #[test]
    fn a_later_frame_reuses_the_image_until_the_package_changes() {
        let (mut pkg, header, _, rid) = package();
        let run = docxcore::image::inline_picture_run(&rid, 9525, 9525, 1, "a.png");
        let cache = RefCell::default();
        let first = Pictures::new(&pkg, &cache).image(&header, &run).unwrap();
        let again = Pictures::new(&pkg, &cache).image(&header, &run).unwrap();
        assert!(Arc::ptr_eq(&first, &again), "a new frame does not rebuild");
        // A part changes: the next frame reads the new media.
        assert!(pkg.set_part("word/media/image2.png", png(9)));
        let changed = Pictures::new(&pkg, &cache).image(&header, &run).unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(changed.bytes, png(9));
        // A clone is the same state; an unrelated read changes nothing.
        let copy = pkg.clone();
        let from_copy = Pictures::new(&copy, &cache).image(&header, &run).unwrap();
        assert!(Arc::ptr_eq(&changed, &from_copy));
    }

    #[test]
    fn dot_dot_and_external_targets_follow_the_package_rules() {
        let (mut pkg, header, _, rid) = package();
        let rels = pkg.part_text("word/_rels/header1.xml.rels").unwrap();
        let up = rels.replace("media/image2.png", "../word/media/image2.png");
        assert!(pkg.set_part("word/_rels/header1.xml.rels", up.into_bytes()));
        assert_eq!(resolve(&pkg, &header, &rid), Some(("png", png(2))));
        let external = rels.replace(
            "Target=\"media/image2.png\"",
            "Target=\"media/image2.png\" TargetMode=\"External\"",
        );
        assert_ne!(external, rels);
        assert!(pkg.set_part("word/_rels/header1.xml.rels", external.into_bytes()));
        assert_eq!(resolve(&pkg, &header, &rid), None);
        let away = rels.replace("media/image2.png", "../../etc/x.png");
        assert!(pkg.set_part("word/_rels/header1.xml.rels", away.into_bytes()));
        assert_eq!(resolve(&pkg, &header, &rid), None);
    }
}
