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
/// [`Package::parts_stamp`]: a change to the package's parts starts a new
/// map, and an image whose bytes are unchanged is carried into it, so gpui
/// keeps its decode (#1639).
#[derive(Default)]
pub struct Cache {
    stamp: u64,
    images: HashMap<PictureKey, Option<Arc<Image>>>,
    /// The map of the stamp before, which a miss may revive by bytes.
    stale: HashMap<PictureKey, Option<Arc<Image>>>,
    /// Images no map holds any more, still decoded in gpui's asset cache
    /// until a host with an `App` releases them ([`Cache::take_released`]).
    retired: Vec<Arc<Image>>,
}

impl Cache {
    /// Start the map of `stamp`: what the stamp before that left unrevived
    /// is retired, and the current map becomes the stale one.
    fn renew(&mut self, stamp: u64) {
        if self.stamp != stamp {
            self.retired
                .extend(self.stale.drain().filter_map(|(_, img)| img));
            self.stale = std::mem::take(&mut self.images);
            self.stamp = stamp;
        }
    }

    /// The images to release: the retired ones, and with `all` (the tab or
    /// its window is going) every one held.
    pub fn take_released(&mut self, all: bool) -> Vec<Arc<Image>> {
        let mut out = std::mem::take(&mut self.retired);
        if all {
            out.extend(self.images.drain().filter_map(|(_, img)| img));
            out.extend(self.stale.drain().filter_map(|(_, img)| img));
        }
        out
    }

    /// The gpui ids of the images this cache may still draw.
    pub fn live_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.images
            .values()
            .chain(self.stale.values())
            .flatten()
            .map(|img| img.id)
    }
}

/// The images of `taken` gpui may forget: a gpui image's id is its bytes'
/// hash, so one another cache still holds (`live`) shares the decode and is
/// kept. Each id once.
pub fn releasable(
    taken: Vec<Arc<Image>>,
    live: &std::collections::HashSet<u64>,
) -> Vec<Arc<Image>> {
    let mut seen = std::collections::HashSet::new();
    taken
        .into_iter()
        .filter(|img| !live.contains(&img.id) && seen.insert(img.id))
        .collect()
}

/// One media part a copy's picture names (#1639): its extension, content
/// type and bytes, as the source package has them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaPart {
    pub ext: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// The media a copy's inline pictures name, by rid, read from the story part
/// they were copied from (#1639): what a paste into another story or package
/// re-adds there. A rid that does not resolve is left out.
pub type Media = HashMap<String, MediaPart>;

/// The bytes behind `rid` in `part`'s relationships, whatever they are.
fn rel_bytes<'p>(pkg: &'p Package, part: &str, rid: &str) -> Option<&'p [u8]> {
    pkg.part(&pkg.part_rel_target(part, rid)?)
}

/// The media part behind `rid` in `part`: any kind (EMF, WMF, TIFF, SVG
/// as well as what the view draws). The extension is the part name's, the
/// content type the package's, else by extension, else by the bytes.
fn media_part(pkg: &Package, part: &str, rid: &str) -> Option<MediaPart> {
    let target = pkg.part_rel_target(part, rid)?;
    let bytes = pkg.part(&target)?;
    let sniffed = docxcore::image::sniff(bytes);
    let name = target.rsplit('/').next().unwrap_or(&target);
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .or_else(|| sniffed.map(|k| k.ext.to_string()))?;
    let content_type = pkg
        .part_content_type(&target)
        .or_else(|| docxcore::image::media_content_type(&ext).map(str::to_string))
        .or_else(|| sniffed.map(|k| k.content_type.to_string()))?;
    Some(MediaPart {
        ext,
        content_type,
        bytes: bytes.to_vec(),
    })
}

/// The media of `clip`'s pictures as `part` of `pkg` resolves them.
pub fn capture(pkg: &Package, part: &str, clip: &docxcore::editor::Clip) -> Media {
    clip.inline_embeds()
        .into_iter()
        .filter_map(|rid| Some((rid.clone(), media_part(pkg, part, &rid)?)))
        .collect()
}

/// Make `clip`'s pictures name `media` from `part` of `dst` (#1639): a rid
/// is kept when it already names the same bytes there, else it takes a
/// relationship of the part to identical bytes, else the media is added
/// ([`Package::add_part_image`], outside any undo step, as Insert Picture
/// does). A rid with no media, or media the part cannot take, is kept.
pub fn carry(dst: &mut Package, part: &str, media: &Media, clip: &mut docxcore::editor::Clip) {
    let mut map = HashMap::new();
    for rid in clip.inline_embeds() {
        let Some(m) = media.get(&rid) else {
            continue;
        };
        let bytes = &m.bytes;
        let same = |dst: &Package, r: &str| rel_bytes(dst, part, r) == Some(bytes.as_slice());
        if same(dst, &rid) {
            continue;
        }
        let found = dst
            .part_rels(part)
            .ids()
            .into_iter()
            .find(|id| same(dst, id))
            .map(str::to_string);
        if let Some(new) =
            found.or_else(|| dst.add_part_media(part, bytes, &m.ext, &m.content_type))
        {
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
    /// decode it; `None` keeps the frame. Only a miss reads the package, and
    /// a miss whose bytes the stamp before held takes that image back.
    pub fn image(&self, part: &str, raw: &str) -> Option<Arc<Image>> {
        let rid = docxcore::image::inline_embed(raw)?;
        let key = (part.to_string(), rid);
        if let Some(hit) = self.cache.borrow().images.get(&key) {
            return hit.clone();
        }
        let mut cache = self.cache.borrow_mut();
        let old = cache.stale.remove(&key).flatten();
        let found = resolve(self.pkg, part, &key.1).and_then(|(ext, bytes)| {
            if let Some(old) = old.as_ref().filter(|o| o.bytes == bytes) {
                return Some(old.clone());
            }
            Some(Arc::new(Image::from_bytes(format_of(ext)?, bytes)))
        });
        if let Some(old) = old.filter(|o| !found.as_ref().is_some_and(|f| Arc::ptr_eq(f, o))) {
            cache.retired.push(old);
        }
        cache.images.insert(key, found.clone());
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

    #[test]
    fn a_stamp_change_keeps_an_image_whose_bytes_did_not_change() {
        let (mut pkg, header, body_rid, rid) = package();
        let run = docxcore::image::inline_picture_run(&rid, 9525, 9525, 1, "a.png");
        let cache = RefCell::<Cache>::default();
        let first = Pictures::new(&pkg, &cache).image(&header, &run).unwrap();
        // Another part changes: the header picture's bytes are the same.
        add(&mut pkg, "word/document.xml", &png(5));
        let again = Pictures::new(&pkg, &cache).image(&header, &run).unwrap();
        assert!(Arc::ptr_eq(&first, &again), "the decode is kept");
        assert!(cache.borrow_mut().take_released(false).is_empty());
        // A picture not drawn after two changes is retired.
        let body = docxcore::image::inline_picture_run(&body_rid, 9525, 9525, 1, "b.png");
        let gone = Pictures::new(&pkg, &cache)
            .image("word/document.xml", &body)
            .unwrap();
        add(&mut pkg, "word/document.xml", &png(3));
        Pictures::new(&pkg, &cache);
        add(&mut pkg, "word/document.xml", &png(4));
        Pictures::new(&pkg, &cache);
        let retired = cache.borrow_mut().take_released(false);
        assert!(retired.iter().any(|i| Arc::ptr_eq(i, &gone)));
        assert!(retired.iter().any(|i| Arc::ptr_eq(i, &first)));
    }

    #[test]
    fn an_image_another_cache_still_holds_is_not_released() {
        let a = Arc::new(Image::from_bytes(ImageFormat::Png, png(1)));
        let b = Arc::new(Image::from_bytes(ImageFormat::Png, png(2)));
        let a_twin = Arc::new(Image::from_bytes(ImageFormat::Png, png(1)));
        assert_eq!(a.id, a_twin.id, "gpui ids are the bytes' hash");
        let live = [b.id].into_iter().collect();
        let out = releasable(vec![a.clone(), b, a_twin], &live);
        assert_eq!(out.len(), 1);
        assert!(Arc::ptr_eq(&out[0], &a));
    }

    #[test]
    fn an_svg_picture_brings_its_png_and_svg_media() {
        let mut src = new_package(Document { body: vec![] });
        let png_rid = add(&mut src, "word/document.xml", &png(1));
        let svg_bytes = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec();
        let svg_rid = src
            .add_part_media("word/document.xml", &svg_bytes, "svg", "image/svg+xml")
            .expect("svg added");
        let run = docxcore::image::inline_picture_run(&png_rid, 9525, 9525, 1, "a.png").replace(
            &format!("<a:blip r:embed=\"{png_rid}\"/>"),
            &format!("<a:blip r:embed=\"{png_rid}\"><a:extLst><a:ext uri=\"{{96DAC541-7B7A-43D3-8B79-37D633B846F1}}\"><asvg:svgBlip xmlns:asvg=\"http://schemas.microsoft.com/office/drawing/2016/SVG/main\" r:embed=\"{svg_rid}\"/></a:ext></a:extLst></a:blip>"),
        );
        let copied = docxcore::editor::Clip {
            paras: vec![vec![docxcore::model::Inline::Raw(run)]],
        };
        let media = capture(&src, "word/document.xml", &copied);
        assert_eq!(media.len(), 2);
        let (mut dst, header, _, _) = package();
        let mut clip = copied.clone();
        carry(&mut dst, &header, &media, &mut clip);
        let rids = clip.inline_embeds();
        assert_eq!(rids.len(), 2);
        assert_eq!(rel_bytes(&dst, &header, &rids[0]), Some(png(1).as_slice()));
        assert_eq!(
            rel_bytes(&dst, &header, &rids[1]),
            Some(svg_bytes.as_slice())
        );
        let ct = dst.part_text("[Content_Types].xml").unwrap();
        assert!(ct.contains("image/svg+xml"), "{ct}");
    }

    #[test]
    fn a_tab_that_saves_no_package_pastes_the_picture_as_it_was() {
        let (src, _, body_rid, _) = package();
        let copied = pic(&body_rid);
        let media = capture(&src, "word/document.xml", &copied);
        assert!(!media.is_empty());
        let mut md = crate::blank_docx_tab("notes.md");
        md.markdown = true;
        let mut clip = copied.clone();
        crate::carry_into_tab(&mut md, &media, &mut clip);
        assert!(md.pkg.is_none());
        assert_eq!(clip, copied);
        // A new Word document is given a package holding the media.
        let mut doc = crate::blank_docx_tab("Document1");
        let mut clip = copied.clone();
        crate::carry_into_tab(&mut doc, &media, &mut clip);
        let pkg = doc.pkg.as_ref().expect("a package");
        let rid = clip.inline_embeds().pop().unwrap();
        assert_eq!(
            resolve(pkg, "word/document.xml", &rid),
            Some(("png", png(1)))
        );
    }

    #[test]
    fn an_emf_picture_pasted_in_a_header_brings_its_part_and_type() {
        let mut src = new_package(Document { body: vec![] });
        let emf = b"\x01\0\0\0 an EMF the view cannot draw".to_vec();
        let rid = src
            .add_part_media("word/document.xml", &emf, "emf", "image/x-emf")
            .expect("added");
        let copied = pic(&rid);
        let media = capture(&src, "word/document.xml", &copied);
        assert_eq!(media[&rid].ext, "emf");
        assert_eq!(media[&rid].content_type, "image/x-emf");
        let mut dst = new_package(Document { body: vec![] });
        let (_, header) = dst.create_hf_part(true, "<w:p/>").unwrap();
        let mut clip = copied.clone();
        carry(&mut dst, &header, &media, &mut clip);
        let new = clip.inline_embeds().pop().unwrap();
        let target = dst.part_rel_target(&header, &new).expect("related");
        assert!(target.ends_with(".emf"), "{target}");
        assert_eq!(dst.part(&target), Some(emf.as_slice()));
        assert_eq!(
            dst.part_content_type(&target).as_deref(),
            Some("image/x-emf")
        );
    }
}
