//! `pdfpages` (#2259): a PDF's pages drawn and read by PdfCraft's
//! `pdfcraft-render`, for the surfaces outside the desktop suite.
//!
//! - Terminal docxy spawns `docxy-pdf` ([`child`], `src/bin/docxy-pdf.rs`):
//!   docxy is in the root workspace (Rust 1.88, on crates.io) and cannot
//!   link PdfCraft, and a child keeps a hostile file out of the editor.
//! - The VS Code webview loads this crate as `pdfpages.wasm` and talks to
//!   [`abi`], a hand-written C ABI in docxwasm's style.
//!
//! Both answer the same three questions of a [`Doc`]: its pages and outline
//! ([`Doc::info`]), a page's pixels ([`Doc::pixels`]) and a page's text in
//! reading order ([`Doc::text`]), as the suite's PDF view asks them
//! (`suite/docxy/src/pdf_view/worker.rs`). No password, link, form or
//! annotation beyond what the renderer draws: those are the suite's.

pub mod abi;
pub mod child;

use pdfcraft_render::{OpenError, PageRenderer, RenderConfig, RenderRequest, RequestKind};
use serde::Serialize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

/// The longest side of a raster, in pixels: the suite's view cap
/// (`pdf_view::MAX_SIDE`); a page asked for larger is drawn at the scale
/// that fits.
pub const MAX_SIDE: f32 = 4096.;
/// The most pixels a raster has: the suite's 16 MP.
pub const MAX_PIXELS: f32 = 16.0e6;
/// The most bookmarks the outline keeps, as the suite's worker.
const MAX_BOOKMARKS: usize = 10_000;

/// What a file that needs a password says: this slice takes none.
pub const LOCKED: &str = "the PDF is protected by a password";
/// What a page that panicked the renderer says.
pub const STOPPED: &str = "the PDF reader stopped on it";

/// One page: its displayed size in points (after `/Rotate` and
/// `/UserUnit`) and its label.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Page {
    pub w: f32,
    pub h: f32,
    pub label: String,
}

/// One bookmark, flattened in outline order: its title, the page it goes
/// to (0-based; none for one that goes elsewhere) and its depth.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Bookmark {
    pub title: String,
    pub page: Option<usize>,
    pub depth: usize,
}

/// A document's pages and outline.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Info {
    pub pages: Vec<Page>,
    pub outline: Vec<Bookmark>,
    pub title: Option<String>,
}

/// Why a document's pages cannot be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// It needs a password.
    Locked,
    /// Anything else, with why.
    Unreadable(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Locked => f.write_str(LOCKED),
            Failure::Unreadable(why) => f.write_str(why),
        }
    }
}

/// A page's pixels: straight (not premultiplied) RGBA, row by row, drawn
/// over white.
#[derive(Clone, Debug, PartialEq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One open PDF: its bytes, its pages once read, and one renderer.
pub struct Doc {
    bytes: Arc<Vec<u8>>,
    info: Option<Result<Info, Failure>>,
    renderer: Option<PageRenderer>,
}

impl Doc {
    pub fn new(bytes: Vec<u8>) -> Doc {
        Doc {
            bytes: Arc::new(bytes),
            info: None,
            renderer: None,
        }
    }

    /// The pages and outline, read once.
    pub fn info(&mut self) -> Result<&Info, Failure> {
        if self.info.is_none() {
            let bytes = self.bytes.clone();
            let read = catch_unwind(AssertUnwindSafe(|| read_info(bytes)))
                .unwrap_or_else(|_| Err(Failure::Unreadable(STOPPED.into())));
            self.info = Some(read);
        }
        match self.info.as_ref() {
            Some(Ok(info)) => Ok(info),
            Some(Err(e)) => Err(e.clone()),
            None => unreachable!("read above"),
        }
    }

    /// Page `page`'s size, or why there is none.
    fn page(&mut self, page: usize) -> Result<Page, String> {
        let info = self.info().map_err(|e| e.to_string())?;
        info.pages
            .get(page)
            .cloned()
            .ok_or_else(|| format!("the PDF has no page {}", page + 1))
    }

    /// Page `page` (0-based) drawn at `scale` pixels a point, or less when
    /// that would pass [`MAX_SIDE`] or [`MAX_PIXELS`].
    pub fn pixels(&mut self, page: usize, scale: f32) -> Result<Raster, String> {
        let size = self.page(page)?;
        let scale = capped_scale(size.w, size.h, scale);
        let r = self.render(RenderRequest {
            page,
            scale,
            ..RenderRequest::default()
        })?;
        if let Some(e) = r.error {
            return Err(e);
        }
        let rgba = straight_rgba(&r.rgba);
        if rgba.is_empty() || rgba.len() != r.width as usize * r.height as usize * 4 {
            return Err("the page drew nothing".into());
        }
        Ok(Raster {
            width: r.width,
            height: r.height,
            rgba,
        })
    }

    /// Page `page`'s text in reading order, lines broken by `\n`.
    pub fn text(&mut self, page: usize) -> Result<String, String> {
        self.page(page)?;
        let r = self.render(RenderRequest {
            page,
            kind: RequestKind::Text,
            ..RenderRequest::default()
        })?;
        match (r.error, r.text) {
            (Some(e), _) => Err(e),
            (None, Some(text)) => Ok(text.plain_text()),
            (None, None) => Err("the page has no text layer".into()),
        }
    }

    fn render(&mut self, req: RenderRequest) -> Result<pdfcraft_render::RenderedPage, String> {
        let bytes = self.bytes.clone();
        let renderer = self
            .renderer
            .get_or_insert_with(|| PageRenderer::new(bytes, RenderConfig::default()));
        // The renderer catches its own panics; this is for the rest of it.
        // On wasm32 a panic aborts (no unwinding), and the host replaces the
        // instance.
        catch_unwind(AssertUnwindSafe(|| renderer.render(req))).map_err(|_| {
            self.renderer = None;
            STOPPED.to_string()
        })
    }

    /// [`Doc::info`] as the wire's JSON: `{"ok":true,"pages":[{w,h,label}],
    /// "outline":[{title,page,depth}],"title"}`, or `{"ok":false,"error",
    /// "locked"}`.
    pub fn info_json(&mut self) -> String {
        match self.info() {
            Ok(info) => serde_json::json!({
                "ok": true,
                "pages": info.pages,
                "outline": info.outline,
                "title": info.title,
            })
            .to_string(),
            Err(e) => failure_json(&e.to_string(), e == Failure::Locked),
        }
    }

    /// [`Doc::text`] as the wire's JSON: `{"ok":true,"text"}` or
    /// `{"ok":false,"error"}`.
    pub fn text_json(&mut self, page: usize) -> String {
        match self.text(page) {
            Ok(text) => serde_json::json!({"ok": true, "text": text}).to_string(),
            Err(e) => failure_json(&e, false),
        }
    }
}

/// `{"ok":false,"error":why,"locked":locked}`.
pub fn failure_json(why: &str, locked: bool) -> String {
    serde_json::json!({"ok": false, "error": why, "locked": locked}).to_string()
}

/// `scale` (1 when it is no positive number) within [`MAX_SIDE`] and
/// [`MAX_PIXELS`] for a page of `w` x `h` points, and within the renderer's
/// own caps.
pub fn capped_scale(w: f32, h: f32, scale: f32) -> f32 {
    let (w, h) = (w.max(1.), h.max(1.));
    let scale = if scale.is_finite() && scale > 0. {
        scale
    } else {
        1.
    };
    let capped = scale
        .min(MAX_SIDE / w.max(h))
        .min((MAX_PIXELS / (w * h)).sqrt());
    pdfcraft_render::effective_scale(w, h, capped)
}

fn read_info(bytes: Arc<Vec<u8>>) -> Result<Info, Failure> {
    let info = match pdfcraft_render::inspect(bytes, None) {
        Ok(info) => info,
        Err(OpenError::NeedsPassword | OpenError::WrongPassword) => return Err(Failure::Locked),
        Err(e) => return Err(Failure::Unreadable(e.to_string())),
    };
    if info.pages.is_empty() {
        return Err(Failure::Unreadable("the PDF has no pages".into()));
    }
    let finite = |v: f32| if v.is_finite() { v.max(1.) } else { 1. };
    let mut outline = Vec::new();
    flatten(&info.outline, 0, &mut outline);
    Ok(Info {
        pages: info
            .pages
            .iter()
            .map(|p| Page {
                w: finite(p.width),
                h: finite(p.height),
                label: p.label.clone(),
            })
            .collect(),
        outline,
        title: info.title.clone().filter(|t| !t.trim().is_empty()),
    })
}

fn flatten(items: &[pdfcraft_render::OutlineItem], depth: usize, out: &mut Vec<Bookmark>) {
    for item in items {
        if out.len() >= MAX_BOOKMARKS {
            return;
        }
        out.push(Bookmark {
            title: item.title.clone(),
            page: item.page,
            depth,
        });
        flatten(&item.children, depth + 1, out);
    }
}

/// The renderer's premultiplied RGBA as straight RGBA. Pages are drawn
/// over white, so alpha is 255 and nothing changes; a pixel with less is
/// un-premultiplied, as the suite's worker does.
pub fn straight_rgba(premultiplied: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(premultiplied.len());
    for px in premultiplied.as_chunks::<4>().0 {
        let a = px[3];
        let un = |c: u8| match a {
            255 => c,
            0 => 0,
            a => ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8,
        };
        out.extend_from_slice(&[un(px[0]), un(px[1]), un(px[2]), a]);
    }
    out
}

#[cfg(test)]
mod tests;
