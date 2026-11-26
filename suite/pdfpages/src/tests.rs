//! `pdfpages` against the suite's PDF corpus (#2259): the references, the
//! text, the sizes and outline, the failures, and the wasm ABI's contract
//! called natively.

use super::*;
use std::path::{Path, PathBuf};

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../corpus/pdf-view/{name}.pdf"))
}

fn open(name: &str) -> Doc {
    Doc::new(std::fs::read(corpus(name)).unwrap())
}

/// Every corpus file with a reference rendering that opens without a
/// password: the suite's `CORPUS` and `HARD` but the user-password files
/// and `source` (the Word fixture, kept in the suite's test).
const REFERENCED: [&str; 12] = [
    "text",
    "columns",
    "vector",
    "scanned",
    "rotated",
    "slides",
    "annots",
    "enc-owner-only",
    "incremental",
    "objstm",
    "damaged-offsets",
    "damaged-noxref",
];

/// The encrypted files that need their user password.
const LOCKED_FILES: [&str; 4] = ["enc-rc4-40", "enc-rc4-128", "enc-aes-128", "enc-aes-256"];

/// The suite's block comparison (`pdf_view::tests::differing`): the share
/// of 3 x 3 blocks whose mean colour differs from the reference's by more
/// than 48 in a channel. `ours` is straight RGBA, or blank white.
fn differing(ours: &[u8], reference: &image::RgbaImage, white: bool) -> f64 {
    const BLOCK: u32 = 3;
    const CHANNEL: u32 = 48;
    let (w, h) = reference.dimensions();
    let (bw, bh) = (w / BLOCK, h / BLOCK);
    let mut bad = 0usize;
    for by in 0..bh {
        for bx in 0..bw {
            let (mut o, mut r) = ([0u32; 3], [0u32; 3]);
            for y in by * BLOCK..(by + 1) * BLOCK {
                for x in bx * BLOCK..(bx + 1) * BLOCK {
                    let p = reference.get_pixel(x, y).0;
                    let i = ((y * w + x) * 4) as usize;
                    let q = if white {
                        [255, 255, 255]
                    } else {
                        [ours[i], ours[i + 1], ours[i + 2]]
                    };
                    for c in 0..3 {
                        o[c] += u32::from(q[c]);
                        r[c] += u32::from(p[c]);
                    }
                }
            }
            let n = BLOCK * BLOCK;
            if (0..3).any(|c| (o[c] / n).abs_diff(r[c] / n) > CHANNEL) {
                bad += 1;
            }
        }
    }
    bad as f64 / (bw * bh) as f64
}

/// Criterion 2: page 1 of each referenced file at one pixel a point matches
/// `pdftoppm -r 72` within the suite's tolerance (at most 1 % of blocks),
/// and a blank page does not.
#[test]
fn page_one_of_every_referenced_file_matches_its_reference() {
    let refs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/pdf-view/ref");
    for name in REFERENCED {
        let reference = image::open(refs.join(format!("{name}-1.png")))
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .to_rgba8();
        let r = open(name)
            .pixels(0, 1.)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((r.width, r.height), reference.dimensions(), "{name}: size");
        assert!(
            r.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255),
            "{name}: opaque"
        );
        let ours = differing(&r.rgba, &reference, false);
        let blank = differing(&r.rgba, &reference, true);
        assert!(
            ours <= 0.01,
            "{name}: {:.2} % of blocks differ",
            ours * 100.
        );
        assert!(blank > 0.01, "{name}: a blank page passes ({blank})");
    }
}

#[test]
fn text_is_read_in_reading_order_with_its_scripts() {
    let t = open("text").text(0).unwrap();
    let lines = [
        "Hello from the first page.",
        "Привет, мир! Съешь же ещё этих мягких французских булок,",
        "Καλημέρα κόσμε. Ξεσκεπάζω την ψυχοφθόρα βδελυγμία.",
    ];
    let at: Vec<usize> = lines
        .iter()
        .map(|l| t.find(l).unwrap_or_else(|| panic!("{l:?} not in {t:?}")))
        .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "reading order: {t}");
    let cols = open("columns").text(0).unwrap();
    let left = cols.rfind("alpha").expect("left column");
    let right = cols.find("Right column starts here").expect("right column");
    assert!(left < right, "the left column first: {cols}");
}

#[test]
fn info_reads_sizes_labels_and_the_outline() {
    let mut slides = open("slides");
    let info = slides.info().unwrap().clone();
    assert_eq!(info.pages.len(), 3);
    for p in &info.pages {
        assert_eq!((p.w.round(), p.h.round()), (960., 540.));
    }
    assert_eq!(info.pages[1].label, "2");
    let titles: Vec<(&str, Option<usize>, usize)> = info
        .outline
        .iter()
        .map(|b| (b.title.as_str(), b.page, b.depth))
        .collect();
    assert!(
        titles.contains(&("Quarterly review", Some(0), 1)),
        "{titles:?}"
    );
    assert!(
        titles.contains(&("Revenue by region", Some(1), 1)),
        "{titles:?}"
    );
    assert!(titles.iter().any(|t| t.0 == "Next steps"), "{titles:?}");
    let rotated = open("rotated").info().unwrap().pages[0].clone();
    assert_eq!((rotated.w.round(), rotated.h.round()), (842., 595.));
}

#[test]
fn unreadable_files_and_garbage_answer_why_never_panic() {
    for bytes in [
        std::fs::read(corpus("damaged-unreadable")).unwrap(),
        b"this is no PDF at all".to_vec(),
        b"%PDF-1.7\n".to_vec(),
        vec![0u8; 4096],
    ] {
        let mut d = Doc::new(bytes);
        assert!(matches!(d.info(), Err(Failure::Unreadable(_))));
        assert!(d.pixels(0, 1.).is_err());
        assert!(d.text(0).is_err());
        let json: serde_json::Value = serde_json::from_str(&d.info_json()).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["locked"], false);
        assert!(json["error"].as_str().is_some_and(|e| !e.is_empty()));
    }
}

#[test]
fn a_file_that_needs_its_password_says_so() {
    for name in LOCKED_FILES {
        let mut d = open(name);
        assert_eq!(d.info().unwrap_err(), Failure::Locked, "{name}");
        let json: serde_json::Value = serde_json::from_str(&d.info_json()).unwrap();
        assert_eq!(json["locked"], true, "{name}");
        assert_eq!(json["error"], LOCKED, "{name}");
    }
    // An owner-only file needs no password to be read.
    assert!(open("enc-owner-only").info().is_ok());
}

#[test]
fn a_page_past_the_last_is_an_error() {
    let mut d = open("text");
    assert_eq!(d.pixels(2, 1.).unwrap_err(), "the PDF has no page 3");
    assert_eq!(d.text(7).unwrap_err(), "the PDF has no page 8");
    assert!(d.text(1).is_ok(), "and the document still answers");
}

#[test]
fn rasters_keep_to_the_view_caps() {
    let mut d = open("slides");
    let r = d.pixels(0, 100.).unwrap();
    assert!(
        r.width.max(r.height) as f32 <= MAX_SIDE + 1.,
        "{} x {}",
        r.width,
        r.height
    );
    assert!((r.width * r.height) as f32 <= MAX_PIXELS * 1.01);
    let r = d.pixels(0, f32::NAN).unwrap();
    assert_eq!(
        (r.width, r.height),
        (960, 540),
        "a scale that is no number is 1"
    );
    assert_eq!(capped_scale(100., 100., -3.), 1.);
}

#[test]
fn straight_rgba_un_premultiplies_what_is_not_opaque() {
    assert_eq!(
        straight_rgba(&[10, 20, 30, 255, 64, 32, 0, 128, 9, 9, 9, 0]),
        vec![10, 20, 30, 255, 128, 64, 0, 128, 0, 0, 0, 0]
    );
}

// ---- the wasm ABI, called natively -------------------------------------------

/// Copy a length-prefixed answer out and free it, as the host does.
fn take(ptr: *mut u8) -> Vec<u8> {
    assert!(!ptr.is_null());
    // SAFETY: `ptr` is an answer of `abi`: a u32 count, then that many bytes.
    unsafe {
        let mut n = [0u8; 4];
        std::ptr::copy_nonoverlapping(ptr, n.as_mut_ptr(), 4);
        let n = u32::from_le_bytes(n) as usize;
        let out = std::slice::from_raw_parts(ptr.add(4), n).to_vec();
        abi::pdf_free(ptr, 4 + n);
        out
    }
}

fn abi_open(bytes: &[u8]) -> u32 {
    let ptr = abi::pdf_alloc(bytes.len());
    // SAFETY: `ptr` is `bytes.len()` writable bytes from `pdf_alloc`, freed
    // with that length after `pdf_open` copied them.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let h = abi::pdf_open(ptr, bytes.len());
        abi::pdf_free(ptr, bytes.len());
        h
    }
}

#[test]
fn the_abi_opens_draws_reads_and_closes() {
    let h = abi_open(&std::fs::read(corpus("slides")).unwrap());
    assert_ne!(h, 0);
    let info: serde_json::Value = serde_json::from_slice(&take(abi::pdf_info(h))).unwrap();
    assert_eq!(info["ok"], true);
    assert_eq!(info["pages"].as_array().unwrap().len(), 3);
    assert_eq!(info["pages"][0]["w"].as_f64().unwrap().round(), 960.);

    let px = take(abi::pdf_render(h, 0, 0.5));
    let w = u32::from_le_bytes(px[0..4].try_into().unwrap());
    let ht = u32::from_le_bytes(px[4..8].try_into().unwrap());
    assert_eq!((w, ht), (480, 270));
    assert_eq!(px.len(), 8 + 480 * 270 * 4);
    assert!(
        px[8..]
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[..3] != [255, 255, 255]),
        "not blank"
    );

    let text: serde_json::Value = serde_json::from_slice(&take(abi::pdf_text(h, 1))).unwrap();
    assert!(
        text["text"].as_str().unwrap().contains("Revenue by region"),
        "{text}"
    );

    let bad = take(abi::pdf_render(h, 9, 1.));
    assert_eq!(&bad[..8], &[0; 8]);
    assert_eq!(
        std::str::from_utf8(&bad[8..]).unwrap(),
        "the PDF has no page 10"
    );

    abi::pdf_close(h);
    let gone: serde_json::Value = serde_json::from_slice(&take(abi::pdf_info(h))).unwrap();
    assert_eq!(gone["error"], "no such document");
    assert_eq!(&take(abi::pdf_render(h, 0, 1.))[..8], &[0; 8]);
}

#[test]
fn the_abi_opens_no_bytes_as_no_document_and_garbage_as_one_that_says_why() {
    // SAFETY: a null pointer with no length is refused before any read.
    assert_eq!(unsafe { abi::pdf_open(std::ptr::null(), 0) }, 0);
    let h = abi_open(b"not a pdf");
    assert_ne!(h, 0);
    let info: serde_json::Value = serde_json::from_slice(&take(abi::pdf_info(h))).unwrap();
    assert_eq!(info["ok"], false);
    let text: serde_json::Value = serde_json::from_slice(&take(abi::pdf_text(h, 0))).unwrap();
    assert_eq!(text["ok"], false);
    abi::pdf_close(h);
    abi::pdf_close(h);
}
