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
    /// Images dropped by a stamp change, still decoded in gpui's asset cache
    /// until [`Cache::release_retired`] runs where an `App` is at hand (#1639).
    retired: Vec<Arc<Image>>,
}

impl Cache {
    /// Forget everything read under an older stamp; its images wait in
    /// `retired` to be released.
    fn renew(&mut self, stamp: u64) {
        if self.stamp != stamp {
            self.retired
                .extend(self.images.drain().filter_map(|(_, img)| img));
            self.stamp = stamp;
        }
    }

    /// Take the images to release: the retired ones, and with `all` (the tab
    /// is closing) every one held.
    fn take_released(&mut self, all: bool) -> Vec<Arc<Image>> {
        let mut out = std::mem::take(&mut self.retired);
        if all {
            out.extend(self.images.drain().filter_map(|(_, img)| img));
        }
        out
    }

    /// Release the images a stamp change dropped from gpui's decoded-asset
    /// cache. The window's atlas tile is not freed (it goes with the window).
    pub fn release_retired(&mut self, cx: &mut gpui::App) {
        for img in self.take_released(false) {
            img.remove_asset(cx);
        }
    }

    /// Release every image the tab drew: the tab is closing. A picture
    /// another tab still shows (same bytes, same asset) decodes again there.
    pub fn release_all(&mut self, cx: &mut gpui::App) {
        for img in self.take_released(true) {
            img.remove_asset(cx);
        }
    }
}

/// The media a copy's inline pictures name, by rid, read from the story part
/// they were copied from (#1639): what a paste into another story or package
/// re-adds there. A rid that does not resolve is left out.
pub type Media = HashMap<String, (&'static str, Vec<u8>)>;

/// The media of `clip`'s pictures as `part` of `pkg` resolves them.
pub fn capture(pkg: &Package, part: &str, clip: &docxcore::editor::Clip) -> Media {
    clip.inline_embeds()
        .into_iter()
        .filter_map(|rid| Some((rid.clone(), resolve(pkg, part, &rid)?)))
        .collect()
}

/// Make `clip`'s pictures name `media` from `part` of `dst` (#1639): a rid
/// is kept when it already resolves there to the same bytes, else it takes
/// an image relationship of the part to identical bytes, else the media is
/// added ([`Package::add_part_image`], outside any undo step, as Insert
/// Picture does). A rid with no media, or media the part cannot take, is
/// kept as it is.
pub fn carry(dst: &mut Package, part: &str, media: &Media, clip: &mut docxcore::editor::Clip) {
    let mut map = HashMap::new();
    for rid in clip.inline_embeds() {
        let Some((_, bytes)) = media.get(&rid) else {
            continue;
        };
        let same = |r: &str| resolve(dst, part, r).is_some_and(|(_, b)| &b == bytes);
        if same(&rid) {
            continue;
        }
        let found = dst
            .part_rels(part)
            .ids()
            .into_iter()
            .find(|id| same(id))
            .map(str::to_string);
        let new = found.or_else(|| {
            let kind = docxcore::image::sniff(bytes)?;
            dst.add_part_image(part, bytes, &kind)
        });
        if let Some(new) = new {
            map.insert(rid, new);
        }
    }
    clip.remap_embeds(&map);
}

/// The pictures one frame draws, through the tab's [`Cache`].
pub struct Pictures<'a> {
    pkg: &'a Package,
    cache: &'a RefCell<Cache>,
}

impl<'a> Pictures<'a> {
    pub fn new(pkg: &'a Package, cache: &'a RefCell<Cache>) -> Self {
        let stamp = pkg.parts_stamp();
        cache.borrow_mut().renew(stamp);
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

/// A story's pictures: the tab's pictures and the part the story came from.
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

    fn pic(rid: &str) -> docxcore::editor::Clip {
        docxcore::editor::Clip {
            paras: vec![vec![docxcore::model::Inline::Raw(
                docxcore::image::inline_picture_run(rid, 9525, 9525, 1, "a.png"),
            )]],
        }
    }

    fn media_parts(pkg: &docxcore::package::Package) -> usize {
        pkg.part_names()
            .into_iter()
            .filter(|n| n.starts_with("word/media/"))
            .count()
    }

    #[test]
    fn a_body_picture_pasted_in_a_header_brings_its_media() {
        let (mut pkg, header, body_rid, head_rid) = package();
        let copied = pic(&body_rid);
        let media = capture(&pkg, "word/document.xml", &copied);
        let before = media_parts(&pkg);
        let mut clip = copied.clone();
        carry(&mut pkg, &header, &media, &mut clip);
        let rid = clip.inline_embeds().pop().unwrap();
        assert_eq!(resolve(&pkg, &header, &rid), Some(("png", png(1))));
        assert_ne!(rid, head_rid, "the header's own picture is not taken");
        assert_eq!(media_parts(&pkg), before + 1);
        // Pasting again (or a redo) reuses that relationship.
        for _ in 0..4 {
            let mut again = copied.clone();
            carry(&mut pkg, &header, &media, &mut again);
            assert_eq!(again.inline_embeds(), vec![rid.clone()]);
        }
        assert_eq!(media_parts(&pkg), before + 1);
    }

    #[test]
    fn a_paste_in_the_same_story_keeps_its_rid_and_adds_nothing() {
        let (mut pkg, _, body_rid, _) = package();
        let copied = pic(&body_rid);
        let media = capture(&pkg, "word/document.xml", &copied);
        let before = pkg.clone();
        let mut clip = copied.clone();
        carry(&mut pkg, "word/document.xml", &media, &mut clip);
        assert_eq!(clip, copied);
        assert_eq!(pkg.parts_stamp(), before.parts_stamp());
    }

    #[test]
    fn a_paste_in_another_package_adds_the_media_there() {
        let mut src = new_package(Document { body: vec![] });
        let body_rid = add(&mut src, "word/document.xml", &png(1));
        let copied = pic(&body_rid);
        let media = capture(&src, "word/document.xml", &copied);
        // The other document's body names a different picture by that rid.
        let mut dst = new_package(Document { body: vec![] });
        let other = add(&mut dst, "word/document.xml", &png(7));
        assert_eq!(other, body_rid);
        let mut clip = copied.clone();
        carry(&mut dst, "word/document.xml", &media, &mut clip);
        let rid = clip.inline_embeds().pop().unwrap();
        assert_ne!(rid, other);
        assert_eq!(
            resolve(&dst, "word/document.xml", &rid),
            Some(("png", png(1)))
        );
        assert_eq!(
            resolve(&dst, "word/document.xml", &other),
            Some(("png", png(7)))
        );
    }

    #[test]
    fn a_picture_that_did_not_resolve_at_copy_is_pasted_as_it_was() {
        let (mut pkg, header, _, _) = package();
        let copied = pic("rId99");
        let media = capture(&pkg, "word/document.xml", &copied);
        assert!(media.is_empty());
        let before = pkg.parts_stamp();
        let mut clip = copied.clone();
        carry(&mut pkg, &header, &media, &mut clip);
        assert_eq!(clip, copied);
        assert_eq!(pkg.parts_stamp(), before);
    }

    #[test]
    fn a_stamp_change_retires_the_images_and_a_close_releases_all() {
        let (mut pkg, header, _, rid) = package();
        let run = docxcore::image::inline_picture_run(&rid, 9525, 9525, 1, "a.png");
        let cache = RefCell::<Cache>::default();
        let first = Pictures::new(&pkg, &cache).image(&header, &run).unwrap();
        assert!(cache.borrow_mut().take_released(false).is_empty());
        assert!(pkg.set_part("word/media/image2.png", png(9)));
        let second = Pictures::new(&pkg, &cache).image(&header, &run).unwrap();
        let retired = cache.borrow_mut().take_released(false);
        assert_eq!(retired.len(), 1);
        assert!(Arc::ptr_eq(&retired[0], &first));
        // Taken once; the current image stays until the tab closes.
        assert!(cache.borrow_mut().take_released(false).is_empty());
        let all = cache.borrow_mut().take_released(true);
        assert_eq!(all.len(), 1);
        assert!(Arc::ptr_eq(&all[0], &second));
        assert!(cache.borrow_mut().take_released(true).is_empty());
    }
}
