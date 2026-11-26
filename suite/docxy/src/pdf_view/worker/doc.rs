//! The child's document (#2245, #2255): the bytes it read, PdfCraft's
//! working copy of them (`pdfcraft-cos`), and the copy pages are drawn
//! from. Slice 1 drew the bytes as they are; now pages are drawn from a
//! display copy, as PdfCraft's own app draws them (`pdfcraft-engine`'s
//! `display_bytes`): comments without an appearance get PdfCraft's
//! (`pdfcraft_annot::with_missing_appearances`), and form fields are drawn
//! from their values where the file leaves that to the reader
//! (`pdfcraft_forms::with_field_appearances`). Neither is ever saved.
//!
//! A filled field changes the working copy only
//! (`pdfcraft_forms::set_value`), which marks the display copy stale: it is
//! built again before the next page or text, so filling ten fields builds
//! it once. A save hands the working copy's bytes over (`write_incremental`:
//! an update appended to the file as read) and changes nothing until the
//! parent says they were written (`saved`): then they are the file.
//!
//! An encrypted file (#2256) opens with the password `unlock` gives: the
//! working copy takes it as typed (user or owner), the renderer the user
//! password, which PdfCraft's renderer alone authenticates for revisions
//! 2-4 (owner authentication recovers it, as `pdfcraft-engine`'s
//! `render_password_for` does).

use super::*;
use pdfcraft_render::{DocInfo, LinkTarget, PageInfo, PageRenderer, RenderConfig};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The most notes `info` lists.
const MAX_NOTES: usize = 10_000;
/// The most links `info` lists.
const MAX_LINKS: usize = 100_000;

pub(super) struct Served {
    /// The file as read, or as last saved.
    bytes: Arc<Vec<u8>>,
    /// `inspect`'s pages: their crop box and rotation turn user space into
    /// view space. Read on first need.
    pages: Option<Vec<PageInfo>>,
    /// The working copy; `None` once it would not open (the file still
    /// draws, with no form).
    cos: Option<pdfcraft_cos::Document>,
    cos_tried: bool,
    /// What pages are drawn and read from; `None` when it is to be built
    /// again (a value changed, or a save was taken in).
    display: Option<Arc<Vec<u8>>>,
    comments: pdfcraft_annot::AppearanceCache,
    appearances: pdfcraft_forms::FieldAppearanceCache,
    renderer: Option<PageRenderer>,
    /// The bytes the last `save` handed over, until `saved` takes them in.
    written: Option<Arc<Vec<u8>>>,
    /// The password `unlock` gave (#2256): as typed, for the working copy,
    /// and the one the renderer takes.
    password: Option<Secret>,
    render_password: Option<Secret>,
}

/// One answer: its line, the bytes after it.
pub(super) type Answered = (Head, Vec<u8>);

impl Served {
    pub(super) fn new(bytes: Arc<Vec<u8>>) -> Served {
        Served {
            bytes,
            pages: None,
            cos: None,
            cos_tried: false,
            display: None,
            comments: Default::default(),
            appearances: Default::default(),
            renderer: None,
            written: None,
            password: None,
            render_password: None,
        }
    }

    /// Answer `op`. A form operation that panics answers why and marks the
    /// answer `fatal`: its working copy may be half changed, so the child
    /// goes and the parent starts another.
    pub(super) fn answer(&mut self, id: u64, op: &Op) -> Answered {
        match op {
            Op::Info => self.info(id),
            Op::Unlock { password } => self.unlock(id, password.clone()),
            Op::Pixels { page, scale } => self.pixels(id, *page, *scale),
            Op::Text { page } => self.text(id, *page),
            Op::SaveTo { .. } => (
                failed(
                    id,
                    "a save's path is the parent's, never the reader's".into(),
                ),
                vec![],
            ),
            op => {
                let op = op.clone();
                match catch_unwind(AssertUnwindSafe(|| self.form_op(id, &op))) {
                    Ok(answered) => answered,
                    Err(_) => {
                        let mut head = failed(id, DIED.into());
                        head.fatal = true;
                        (head, vec![])
                    }
                }
            }
        }
    }

    // ---- reading ----------------------------------------------------------------

    /// Open the file with `password` (#2256): everything read so far goes,
    /// the working copy opens with it first (a password it refuses answers
    /// locked), and the renderer gets the user password owner
    /// authentication recovered, or `password` itself. A file the working
    /// copy cannot read for another reason is left to the renderer, with
    /// `password` as typed. Then the answer is `info`'s.
    fn unlock(&mut self, id: u64, password: Secret) -> Answered {
        self.pages = None;
        self.cos = None;
        self.cos_tried = false;
        self.display = None;
        self.renderer = None;
        self.comments = Default::default();
        self.appearances = Default::default();
        self.written = None;
        self.password = None;
        self.render_password = None;
        let opened = catch_unwind(AssertUnwindSafe(|| {
            pdfcraft_cos::Document::open_with_password(self.bytes.clone(), Some(password.as_str()))
        }));
        let render = match opened {
            Ok(Err(
                pdfcraft_cos::CosError::NeedsPassword | pdfcraft_cos::CosError::WrongPassword,
            )) => {
                return (locked(id, Locked::Wrong), vec![]);
            }
            Ok(Ok(cos)) => {
                let recovered = render_password(&cos);
                self.cos = Some(cos);
                self.cos_tried = true;
                recovered.unwrap_or_else(|| password.clone())
            }
            Ok(Err(_)) | Err(_) => password.clone(),
        };
        self.password = Some(password);
        self.render_password = Some(render);
        let answered = self.info(id);
        if answered.0.locked.is_some() {
            self.password = None;
            self.render_password = None;
            self.cos = None;
            self.cos_tried = false;
            return (locked(id, Locked::Wrong), vec![]);
        }
        answered
    }

    fn render_pw(&self) -> Option<&str> {
        self.render_password.as_ref().map(Secret::as_str)
    }

    fn info(&mut self, id: u64) -> Answered {
        let info = match pdfcraft_render::inspect(self.bytes.clone(), self.render_pw()) {
            Ok(info) if info.pages.is_empty() => {
                return (failed(id, "the PDF has no pages".into()), vec![]);
            }
            Ok(info) => info,
            Err(pdfcraft_render::OpenError::NeedsPassword) => {
                return (locked(id, Locked::Needs), vec![]);
            }
            Err(pdfcraft_render::OpenError::WrongPassword) => {
                return (locked(id, Locked::Wrong), vec![]);
            }
            Err(e) => return (failed(id, e.to_string()), vec![]),
        };
        self.pages = Some(info.pages.clone());
        let mut meta = doc_meta(&info);
        meta.links = links(&info);
        // The form and the comments' Hidden flags need the working copy; a
        // file it does not open still draws and reads.
        let cos = catch_unwind(AssertUnwindSafe(|| self.cos().is_some())).unwrap_or(false);
        let cos = if cos { self.cos.as_ref() } else { None };
        meta.notes = notes(&info, cos);
        meta.form = cos
            .and_then(|cos| catch_unwind(AssertUnwindSafe(|| form_meta(cos, &info.pages))).ok())
            .unwrap_or_default();
        meta.security = cos.and_then(|cos| {
            catch_unwind(AssertUnwindSafe(|| security(cos)))
                .ok()
                .flatten()
        });
        if !cos.is_none_or(may_fill) {
            read_only(&mut meta.form.fields);
        }
        (
            Head {
                id,
                info: Some(meta),
                ..Head::default()
            },
            vec![],
        )
    }

    /// The working copy, opened on first need.
    fn cos(&mut self) -> Option<&pdfcraft_cos::Document> {
        self.cos_mut().map(|c| &*c)
    }

    fn cos_mut(&mut self) -> Option<&mut pdfcraft_cos::Document> {
        if !self.cos_tried {
            self.cos_tried = true;
            let password = self.password.as_ref().map(Secret::as_str);
            self.cos =
                pdfcraft_cos::Document::open_with_password(self.bytes.clone(), password).ok();
        }
        self.cos.as_mut()
    }

    /// The pages' geometry: `inspect`'s, read now if `info` was not asked.
    fn pages(&mut self) -> Vec<PageInfo> {
        if self.pages.is_none() {
            self.pages = Some(
                pdfcraft_render::inspect(self.bytes.clone(), self.render_pw())
                    .map(|i| i.pages)
                    .unwrap_or_default(),
            );
        }
        self.pages.clone().unwrap_or_default()
    }

    /// What pages are drawn from, built when stale. A build that fails (or
    /// panics) draws the working copy's own bytes.
    fn display(&mut self) -> Arc<Vec<u8>> {
        if let Some(d) = &self.display {
            return d.clone();
        }
        let built = catch_unwind(AssertUnwindSafe(|| self.build_display()))
            .ok()
            .flatten();
        let shown = built.unwrap_or_else(|| self.bytes.clone());
        self.display = Some(shown.clone());
        self.renderer = None;
        shown
    }

    fn build_display(&mut self) -> Option<Arc<Vec<u8>>> {
        self.cos();
        let Served {
            cos,
            comments,
            appearances,
            ..
        } = self;
        let cos = cos.as_ref()?;
        let with_comments = pdfcraft_annot::with_missing_appearances_cached(cos, comments);
        let with_fields = pdfcraft_forms::with_field_appearances_cached(
            with_comments.as_ref().unwrap_or(cos),
            appearances,
        );
        let shown = match (&with_fields, &with_comments) {
            (Some(d), _) | (None, Some(d)) => d,
            (None, None) if cos.is_modified() => cos,
            // Nothing to add and nothing changed: the bytes as they are.
            (None, None) => return None,
        };
        pdfcraft_cos::write_incremental(shown, &pdfcraft_cos::SaveOptions::default())
            .ok()
            .map(Arc::new)
    }

    fn render(&mut self, req: pdfcraft_render::RenderRequest) -> pdfcraft_render::RenderedPage {
        let display = self.display();
        // The display copy is written with the file's own encryption: it
        // takes the same password.
        let config = RenderConfig {
            password: self.render_pw().map(Arc::from),
            ..RenderConfig::default()
        };
        self.renderer
            .get_or_insert_with(|| PageRenderer::new(display, config))
            .render(req)
    }

    fn pixels(&mut self, id: u64, page: usize, scale: f32) -> Answered {
        let scale = if scale.is_finite() && scale > 0. {
            scale
        } else {
            1.
        };
        let r = self.render(pdfcraft_render::RenderRequest {
            page,
            scale,
            ..Default::default()
        });
        if let Some(e) = r.error {
            return (failed(id, e), vec![]);
        }
        let bgra = straight_bgra(&r.rgba);
        if bgra.len() != r.width as usize * r.height as usize * 4 || bgra.is_empty() {
            return (failed(id, "the page drew nothing".into()), vec![]);
        }
        (
            Head {
                id,
                w: r.width,
                h: r.height,
                len: bgra.len(),
                ..Head::default()
            },
            bgra,
        )
    }

    fn text(&mut self, id: u64, page: usize) -> Answered {
        let r = self.render(pdfcraft_render::RenderRequest {
            page,
            kind: pdfcraft_render::RequestKind::Text,
            ..Default::default()
        });
        match (r.error, r.text) {
            (Some(e), _) => (failed(id, e), vec![]),
            (None, Some(text)) => (
                Head {
                    id,
                    text: Some(text_wire(&text)),
                    ..Head::default()
                },
                vec![],
            ),
            (None, None) => (failed(id, "the page has no text layer".into()), vec![]),
        }
    }

    // ---- the form ---------------------------------------------------------------

    fn form_op(&mut self, id: u64, op: &Op) -> Answered {
        let pages = self.pages();
        // A file whose permissions withhold filling in its form is never
        // filled (#2256), whatever asks: the view shows its fields read-only.
        let refused = matches!(op, Op::SetField { .. } | Op::SetFields { .. })
            && !self.cos().is_none_or(may_fill);
        if refused {
            return (failed(id, NO_FILLING.into()), vec![]);
        }
        let result = match op {
            Op::SetField { name, value } => self.fill(&pages, |cos| {
                pdfcraft_forms::set_value(cos, name, &to_value(value)).map_err(|e| e.to_string())
            }),
            Op::SetFields { values } => self.fill(&pages, |cos| {
                let values: Vec<(String, pdfcraft_forms::FieldValue)> = values
                    .iter()
                    .map(|(n, v)| (n.clone(), to_value(v)))
                    .collect();
                let results =
                    pdfcraft_forms::set_values(cos, &values).map_err(|e| e.to_string())?;
                results
                    .into_iter()
                    .find_map(Result::err)
                    .map_or(Ok(()), |e| Err(e.to_string()))
            }),
            Op::Fields => Ok(FormState {
                fields: self
                    .cos()
                    .map(|c| {
                        let mut f = fields(c, &pages);
                        if !may_fill(c) {
                            read_only(&mut f);
                        }
                        f
                    })
                    .unwrap_or_default(),
                pages: Vec::new(),
            }),
            Op::Save => return self.save(id),
            Op::Saved => return self.saved(id),
            _ => Err("not a form operation".into()),
        };
        match result {
            Ok(form) => (
                Head {
                    id,
                    form: Some(form),
                    ..Head::default()
                },
                vec![],
            ),
            Err(why) => (failed(id, why), vec![]),
        }
    }

    /// Change the working copy with `f`; the answer names the pages whose
    /// fields changed (calculated fields too, as `set_value` recalculates).
    fn fill(
        &mut self,
        pages: &[PageInfo],
        f: impl FnOnce(&mut pdfcraft_cos::Document) -> Result<(), String>,
    ) -> Result<FormState, String> {
        let cos = self
            .cos_mut()
            .ok_or("the PDF could not be read for its form")?;
        let before = fields(cos, pages);
        f(cos)?;
        let after = fields(cos, pages);
        self.display = None;
        self.renderer = None;
        Ok(FormState {
            pages: changed_pages(&before, &after),
            fields: after,
        })
    }

    /// The bytes a save writes: the file as it is when nothing changed, else
    /// the working copy as an update appended to it (written in full when it
    /// cannot be: a rebuilt cross-reference table). Password fields' values
    /// are never written (ISO 32000-2 Table 231); the working copy keeps them.
    fn save(&mut self, id: u64) -> Answered {
        let bytes = match self.cos() {
            Some(cos) if cos.is_modified() => {
                let written = if pdfcraft_forms::has_password_values(cos) {
                    let mut copy = cos.clone();
                    pdfcraft_forms::clear_password_values(&mut copy)
                        .map_err(|e| e.to_string())
                        .and_then(|_| {
                            pdfcraft_cos::write_incremental(
                                &copy,
                                &pdfcraft_cos::SaveOptions::default(),
                            )
                            .map_err(|e| e.to_string())
                        })
                } else {
                    pdfcraft_cos::write_incremental(cos, &pdfcraft_cos::SaveOptions::default())
                        .map_err(|e| e.to_string())
                };
                match written {
                    Ok(b) => Arc::new(b),
                    Err(e) => {
                        return (
                            failed(id, format!("the PDF could not be written: {e}")),
                            vec![],
                        );
                    }
                }
            }
            _ => self.bytes.clone(),
        };
        if bytes.len() > MAX_SAVE_BYTES {
            return (failed(id, "the PDF is too large to save".into()), vec![]);
        }
        self.written = Some(bytes.clone());
        (
            Head {
                id,
                bytes: bytes.len(),
                ..Head::default()
            },
            bytes.to_vec(),
        )
    }

    /// The last save's bytes are the file now: the working copy opens on
    /// them (a next save appends to them), and keeps the passwords typed.
    fn saved(&mut self, id: u64) -> Answered {
        let Some(bytes) = self.written.take() else {
            return (failed(id, "nothing was saved".into()), vec![]);
        };
        let typed = self
            .cos
            .as_ref()
            .map(pdfcraft_forms::password_values)
            .unwrap_or_default();
        // An encrypted file's save has its encryption: the open file's key
        // reads it, and no password is needed (#2256).
        let reopened = match self.cos.as_ref() {
            Some(cos) => cos.open_with_same_credentials(bytes.clone()),
            None => pdfcraft_cos::Document::open_with_password(
                bytes.clone(),
                self.password.as_ref().map(Secret::as_str),
            ),
        };
        let mut reopened = match reopened {
            Ok(d) => d,
            Err(e) => {
                return (
                    failed(id, format!("the saved PDF could not be read: {e}")),
                    vec![],
                );
            }
        };
        if !typed.is_empty() {
            let _ = pdfcraft_forms::restore_password_values(&mut reopened, &typed);
        }
        self.bytes = bytes;
        self.cos = Some(reopened);
        self.cos_tried = true;
        self.display = None;
        self.renderer = None;
        self.comments = Default::default();
        self.appearances = Default::default();
        (
            Head {
                id,
                ..Head::default()
            },
            vec![],
        )
    }
}

/// What a fill of a form the file does not let be filled answers (#2256).
pub(crate) const NO_FILLING: &str = "this PDF's permissions do not allow filling in its form";

fn locked(id: u64, why: Locked) -> Head {
    Head {
        id,
        locked: Some(why),
        ..Head::default()
    }
}

/// The password the renderer takes for a file `cos` opened (#2256): the
/// user password owner authentication recovered for revisions 2-4, whose
/// owner password the renderer does not authenticate. As
/// `pdfcraft-engine`'s `recovered_render_password`: UTF-8 when it decodes,
/// else each byte in PDFDocEncoding.
fn render_password(cos: &pdfcraft_cos::Document) -> Option<Secret> {
    let user = cos.security()?.recovered_user_password()?;
    let text = match std::str::from_utf8(&user) {
        Ok(text) => text.to_owned(),
        Err(_) => user.iter().map(|b| pdfcraft_cos::pdfdoc_char(*b)).collect(),
    };
    Some(Secret::new(text))
}

/// Whether the password `cos` opened with lets its form be filled (an
/// unencrypted file does; the owner password always does).
fn may_fill(cos: &pdfcraft_cos::Document) -> bool {
    cos.permissions().is_none_or(|p| p.fill_forms())
}

fn read_only(fields: &mut [FieldMeta]) {
    for f in fields {
        f.read_only = true;
        for w in &mut f.widgets {
            w.read_only = true;
        }
    }
}

/// An encrypted file's cipher and permissions (#2256).
fn security(cos: &pdfcraft_cos::Document) -> Option<SecurityMeta> {
    let handler = cos.security()?;
    let p = handler.permissions();
    let d = handler.dict();
    let stream = d
        .crypt_filters
        .iter()
        .find(|(name, _)| *name == d.stm_f)
        .map(|(_, m)| *m);
    let method = match (d.r, stream) {
        (5.., _) | (_, Some(pdfcraft_cos::CryptMethod::Aes256)) => "AES 256-bit".to_string(),
        (4, Some(pdfcraft_cos::CryptMethod::Aes128)) => "AES 128-bit".to_string(),
        (2, _) => "RC4 40-bit".to_string(),
        _ => format!("RC4 {}-bit", d.length_bits),
    };
    Some(SecurityMeta {
        method,
        owner: p.owner,
        print: p.print(),
        copy: p.copy(),
        modify: p.modify(),
        annotate: p.annotate(),
        fill_forms: p.fill_forms(),
    })
}

fn to_value(v: &FieldValue) -> pdfcraft_forms::FieldValue {
    match v {
        FieldValue::Text { text } => pdfcraft_forms::FieldValue::Text(text.clone()),
        FieldValue::Check { on } => pdfcraft_forms::FieldValue::Check(*on),
        FieldValue::Widget { index } => pdfcraft_forms::FieldValue::Widget(*index),
        FieldValue::Choice { picked } => pdfcraft_forms::FieldValue::ChoiceIndices(picked.clone()),
    }
}

/// The pages whose widgets show something else between `before` and
/// `after` (every page with a widget when the form changed shape).
pub(super) fn changed_pages(before: &[FieldMeta], after: &[FieldMeta]) -> Vec<usize> {
    let mut pages: Vec<usize> = if before.len() != after.len() {
        after
            .iter()
            .flat_map(|f| f.widgets.iter().filter_map(|w| w.page))
            .collect()
    } else {
        before
            .iter()
            .zip(after)
            .filter(|(b, a)| b != a)
            .flat_map(|(b, a)| b.widgets.iter().chain(&a.widgets).filter_map(|w| w.page))
            .collect()
    };
    pages.sort_unstable();
    pages.dedup();
    pages
}

// ---- turning PdfCraft's model into the view's -------------------------------------

fn finite4(r: [f32; 4]) -> [f32; 4] {
    r.map(finite)
}

/// A user-space rectangle of `page` in view space, normalized.
fn view_rect(pages: &[PageInfo], page: usize, r: [f32; 4]) -> [f32; 4] {
    let Some(p) = pages.get(page) else {
        return [0.; 4];
    };
    let a = p.user_to_view(r[0], r[1]);
    let b = p.user_to_view(r[2], r[3]);
    finite4([
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[0].max(b[0]),
        a[1].max(b[1]),
    ])
}

fn links(info: &DocInfo) -> Vec<LinkMeta> {
    info.links
        .iter()
        .take(MAX_LINKS)
        .filter_map(|l| {
            let p = info.pages.get(l.page)?;
            let quads = l
                .quads
                .iter()
                .map(|q| {
                    let mut out = [0.; 8];
                    for i in 0..4 {
                        let [x, y] = p.user_to_view(q[2 * i], q[2 * i + 1]);
                        out[2 * i] = finite(x);
                        out[2 * i + 1] = finite(y);
                    }
                    out
                })
                .collect();
            Some(LinkMeta {
                page: l.page,
                rect: view_rect(&info.pages, l.page, l.rect),
                quads,
                go: link_go(&info.pages, &l.target),
            })
        })
        .collect()
}

fn link_go(pages: &[PageInfo], target: &LinkTarget) -> LinkGo {
    use pdfcraft_render::DestView;
    match target {
        LinkTarget::Uri(uri) => LinkGo::Uri { uri: uri.clone() },
        LinkTarget::Page(page, view) => {
            let Some(p) = pages.get(*page) else {
                return LinkGo::Other {
                    what: "a page this file does not have".into(),
                };
            };
            // The point the view goes to, by the coordinates the
            // destination gives (user space, either may be left as it is).
            // A destination with no point goes to the page's displayed top
            // (no coordinate: the view's own), whatever its /Rotate: the
            // crop box's top edge is its side or bottom when turned.
            let (ux, uy, zoom, fit_width) = match *view {
                DestView::Top | DestView::Fit => (None, None, None, false),
                DestView::Xyz { left, top, zoom } => (left, top, zoom, false),
                DestView::FitH { top } => (None, top, None, true),
                DestView::FitV { left } => (left, None, None, false),
                DestView::FitR { rect } => (Some(rect[0]), Some(rect[3]), None, false),
            };
            let [fx, fy] = p.dest_fraction(ux, uy, 0);
            LinkGo::Page {
                page: *page,
                x: fx.map(|f| finite(f * p.width)),
                y: fy.map(|f| finite(f * p.height)),
                zoom: zoom.filter(|z| z.is_finite() && *z > 0.),
                fit_width,
            }
        }
        LinkTarget::JavaScript(_) => LinkGo::Other {
            what: "a script".into(),
        },
        LinkTarget::SetLayers { .. } => LinkGo::Other {
            what: "a change of the document's layers".into(),
        },
        LinkTarget::RemoteGoTo { launch, file, .. } => LinkGo::Other {
            what: if *launch {
                format!("a launch of {file}")
            } else {
                format!("a place in another file, {file}")
            },
        },
        LinkTarget::Other(what) => LinkGo::Other {
            what: format!("an action docxy does not take ({what})"),
        },
    }
}

/// The comments whose text shows on hover: comments of their own (not
/// replies or statuses) with text, but free text (its text is on the page)
/// and those the file hides (`/F` Hidden or NoView).
fn notes(info: &DocInfo, cos: Option<&pdfcraft_cos::Document>) -> Vec<NoteMeta> {
    let page_dicts = cos.map(pdfcraft_model::pages).unwrap_or_default();
    info.annotations
        .iter()
        .filter(|a| a.is_comment() && a.subtype != "FreeText")
        .filter_map(|a| {
            let text = a.contents.as_deref()?.trim();
            if text.is_empty() {
                return None;
            }
            if let Some(cos) = cos
                && hidden(cos, page_dicts.get(a.page), a.index)
            {
                return None;
            }
            let p = info.pages.get(a.page)?;
            Some(NoteMeta {
                page: a.page,
                rect: view_rect(&info.pages, a.page, a.shown_rect(p.rotation)),
                subtype: a.subtype.clone(),
                author: a.author.clone().filter(|s| !s.trim().is_empty()),
                date: a.modified.clone().filter(|d| !d.trim().is_empty()),
                contents: text.to_string(),
            })
        })
        .take(MAX_NOTES)
        .collect()
}

/// Whether annotation `index` of `page` is Hidden or NoView (`/F` bits 2
/// and 6): never drawn, so it shows no pop-up either.
fn hidden(cos: &pdfcraft_cos::Document, page: Option<&pdfcraft_model::Page>, index: usize) -> bool {
    let Some(page) = page else { return false };
    let Some(annots) = page.dict.get(b"Annots").map(|a| cos.resolve(a)) else {
        return false;
    };
    let Some(a) = annots
        .as_array()
        .and_then(|a| a.get(index))
        .map(|a| cos.resolve(a))
    else {
        return false;
    };
    let flags = a
        .as_dict()
        .and_then(|d| d.get(b"F"))
        .and_then(|f| cos.resolve(f).as_int())
        .unwrap_or(0);
    flags & (2 | 32) != 0
}

fn form_meta(cos: &pdfcraft_cos::Document, pages: &[PageInfo]) -> FormMeta {
    let all = pdfcraft_forms::fields(cos);
    let signed = all.iter().any(|f| {
        f.kind == pdfcraft_forms::FieldKind::Signature
            && cos
                .get(f.obj)
                .as_dict()
                .and_then(|d| d.get(b"V"))
                .is_some_and(|v| cos.resolve(v).as_dict().is_some())
    });
    FormMeta {
        fields: all.iter().map(|f| field_meta(f, pages)).collect(),
        signed,
        incremental: !cos.reconstructed() && !cos.encryption_changed() && !cos.full_save_required(),
    }
}

fn fields(cos: &pdfcraft_cos::Document, pages: &[PageInfo]) -> Vec<FieldMeta> {
    pdfcraft_forms::fields(cos)
        .iter()
        .map(|f| field_meta(f, pages))
        .collect()
}

fn field_meta(f: &pdfcraft_forms::Field, pages: &[PageInfo]) -> FieldMeta {
    use pdfcraft_forms::{FieldKind as K, flags};
    let rect32 = |r: [f64; 4]| r.map(|v| v as f32);
    FieldMeta {
        name: f.name.clone(),
        kind: match f.kind {
            K::Text => FieldKind::Text,
            K::CheckBox => FieldKind::Check,
            K::Radio => FieldKind::Radio,
            K::PushButton => FieldKind::Button,
            K::Combo => FieldKind::Combo,
            K::List => FieldKind::List,
            K::Signature => FieldKind::Signature,
        },
        value: f.value.clone(),
        picked: f.selected.clone(),
        options: f.options.clone(),
        read_only: f.read_only(),
        required: f.has(flags::REQUIRED),
        multiline: f.has(flags::MULTILINE),
        password: f.has(flags::PASSWORD),
        comb: f.is_comb(),
        edit: f.has(flags::EDIT),
        multi: f.has(flags::MULTI_SELECT),
        max_len: f.max_len,
        tooltip: f.tooltip.clone().filter(|t| !t.trim().is_empty()),
        font_size: da_size(&f.da),
        widgets: f
            .widgets
            .iter()
            .enumerate()
            .map(|(i, w)| WidgetMeta {
                page: w.page,
                rect: w
                    .page
                    .map_or([0.; 4], |p| view_rect(pages, p, rect32(w.rect))),
                on_state: w.on_state.clone(),
                on: f.widget_on(i),
                hidden: w.hidden,
                read_only: w.read_only || f.read_only(),
                tab: w.tab,
                list: w.list.as_ref().map(|l| ListRowsMeta {
                    top: l.top,
                    inset: l.inset as f32,
                    row: l.row as f32,
                }),
            })
            .collect(),
    }
}

/// The text size a default appearance string (`/DA`) sets: the operand of
/// its last `Tf` (0, or none: fit the widget).
pub(super) fn da_size(da: &str) -> f32 {
    let words: Vec<&str> = da.split_whitespace().collect();
    words
        .iter()
        .rposition(|w| *w == "Tf")
        .and_then(|i| i.checked_sub(1))
        .and_then(|i| words.get(i))
        .and_then(|w| w.parse::<f32>().ok())
        .filter(|s| s.is_finite() && *s >= 0.)
        .unwrap_or(0.)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfcraft_render::DestView;

    fn page(rotation: u16) -> PageInfo {
        let (w, h) = if rotation.is_multiple_of(180) {
            (595., 842.)
        } else {
            (842., 595.)
        };
        PageInfo {
            width: w,
            height: h,
            label: "1".into(),
            crop: [0., 0., 595., 842.],
            rotation,
        }
    }

    /// r1: a destination with no point (/Fit, a bare page, /FitH null) goes
    /// to the page's displayed top on every /Rotate: no coordinate, never
    /// the crop box's top edge, which a turned page shows at its side or
    /// bottom.
    #[test]
    fn a_destination_with_no_point_goes_to_the_top_on_any_rotation() {
        for rotation in [0, 90, 180, 270] {
            let pages = [page(rotation)];
            for view in [DestView::Top, DestView::Fit, DestView::FitH { top: None }] {
                match link_go(&pages, &LinkTarget::Page(0, view)) {
                    LinkGo::Page {
                        x: None, y: None, ..
                    } => {}
                    other => panic!("{rotation} {view:?}: {other:?}"),
                }
            }
            // A /FitH with a top keeps it: on a page turned upside down,
            // 742 points up shows 100 points from the top.
            if rotation == 180 {
                match link_go(
                    &pages,
                    &LinkTarget::Page(0, DestView::FitH { top: Some(742.) }),
                ) {
                    LinkGo::Page { y: Some(y), .. } => assert!((y - 742.).abs() < 0.01, "{y}"),
                    other => panic!("{other:?}"),
                }
            }
        }
    }
}
