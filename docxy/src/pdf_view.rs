//! Terminal docxy's PDF viewer (#2259): `docxy <file.pdf>` shows the pages
//! as they look, read-only, through the terminal's picture protocol (the
//! `Picker` docxy draws inline pictures with: kitty, iTerm2, Sixel), or each
//! page's text in reading order.
//!
//! The pages are drawn and read by `docxy-pdf` ([`child::Child`]), the
//! desktop suite's PDF renderer (PdfCraft's `pdfcraft-render`) in a child
//! process: docxy cannot link PdfCraft (it needs a newer Rust, and is a git
//! dependency crates.io refuses), and a hostile file costs the child, never
//! the editor. Questions go to the child one at a time on a thread of its
//! own ([`Engine`]), so a slow page never freezes the screen; a page the
//! child fails on says why and is not asked again. Without the child (none
//! found, or it cannot be started), or when it gives a reason it cannot open
//! the file, the text importer (#951) shows the file's text as one stream
//! and the status says why the pages are not drawn. When the child dies or
//! runs out of time opening the file, nothing else reads it in docxy's own
//! process: the viewer says why and shows no text. Every text from the file
//! is cleaned of control characters before it reaches a cell
//! ([`clean_text`]).
//!
//! It is its own small loop beside the editor's `App`: nothing here edits,
//! saves or converts. It publishes the editor's control server with two
//! verbs, `field-read` (the counters, and its Go to page prompt) and
//! `pdf-read` (`{"until_idle":true}` waits up to 5 s for nothing to be
//! pending), for the UI harness and agents.

pub(crate) mod child;
#[cfg(test)]
mod tests;

use child::{Ans, Child, Killer, Meta, Raster, Req, Source, TextOnly};
use ctlcore::field::{Field, InputCounts};
use ctlcore::json::Json;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use unicode_width::UnicodeWidthChar;

/// The viewer's one text input: its Go to page prompt (`g`).
pub(crate) const INPUTS: &[&str] = &["docxy-pdf-page"];
/// The longest `pdf-read {"until_idle":true}` waits.
const UNTIL_IDLE: std::time::Duration = std::time::Duration::from_secs(5);
/// How many rasters are kept: the page shown, the next, and a turn back.
const RASTERS_KEPT: usize = 4;
/// What says how to get the pages drawn.
pub(crate) const HOW_TO_GET_PAGES: &str = "pages need docxy-pdf (in the release downloads beside \
     docxy; or cargo build --release --manifest-path suite/Cargo.toml -p pdfpages)";
/// What a file that needs a password says: no prompt in the terminal.
pub(crate) const LOCKED: &str =
    "This PDF is protected by a password: open it in the docxy suite, which asks for it.";

/// Whether `path` is a PDF by its first bytes, as the importer and the
/// suite sniff them (junk before `%PDF-` within the first 1 KB is a PDF; a
/// page or an RTF that only mentions it is not). By its bytes alone: a file
/// named `.pdf` without the header is not one here, and goes the old way.
pub(crate) fn is_pdf(path: &Path) -> bool {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::with_capacity(4096);
    if f.take(4096).read_to_end(&mut head).is_err() || head.is_empty() {
        return false;
    }
    docxcore::import::sniff(&head, "") == docxcore::import::Format::Pdf
}

/// Whether `docxy <path>` opens the PDF viewer: a PDF by its bytes, whatever
/// its name, but never a Markdown file or a web page (which may mention
/// `%PDF-` near their top).
pub(crate) fn opens_as_pdf(path: &str) -> bool {
    crate::format_for(path) == crate::DocFormat::Docx && is_pdf(Path::new(path))
}

/// Why `docxy <file.pdf> --pdf|--md|--docx|--html <out>` refuses.
pub(crate) fn headless_refusal(path: &str) -> String {
    format!("{path} is a PDF: the terminal shows it read-only; convert it in the docxy suite")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The page drawn.
    Image,
    /// The page's text.
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Fit {
    /// The whole page in the body.
    Page,
    /// The body's width; Up/Down scroll the rest.
    Width,
}

/// A raster asked for: a page fitted to a box of body pixels (`h` 0 when
/// the page fits the width).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Want {
    page: usize,
    fit: Fit,
    w: u32,
    h: u32,
}

/// What the loop does after an answer.
#[derive(Debug, PartialEq)]
pub(crate) enum Next {
    Nothing,
    /// The child cannot show the file: read its text instead, saying why;
    /// `missing` when there is no child to run (the status says how to get
    /// one).
    TextOnly {
        why: String,
        missing: bool,
    },
}

/// The protocol last encoded, and what it encodes.
type ProtoKey = (Want, usize, Rect);

pub(crate) struct Viewer {
    pub(crate) name: String,
    pub(crate) meta: Option<Meta>,
    pub(crate) page: usize,
    pub(crate) mode: Mode,
    pub(crate) fit: Fit,
    /// Rows scrolled in the body: text lines, or a fit-width page's rows.
    pub(crate) scroll: usize,
    /// The pages come from the text importer, not the child.
    pub(crate) text_only: bool,
    /// The file shows nothing: why (a password, an unreadable file).
    pub(crate) unopened: Option<String>,
    pub(crate) status: String,
    /// The Go to page prompt's digits, while it is open.
    pub(crate) goto: Option<String>,
    pub(crate) counts: InputCounts,
    texts: HashMap<usize, Result<String, String>>,
    rasters: Vec<(Want, Raster)>,
    /// Pages the child could not draw, and why: never asked again.
    failed: BTreeMap<usize, String>,
    asked_info: bool,
    pending: Option<(Req, Option<Want>)>,
    /// The body's area as last drawn.
    body: Rect,
    /// A cell's size in pixels.
    font: (u16, u16),
    picker: Option<Picker>,
    proto: Option<(ProtoKey, Protocol)>,
    /// Where the Go to page prompt's digits were last drawn.
    goto_at: Option<(u16, u16)>,
    /// The body's text rows as last drawn.
    shown: Vec<String>,
    /// Why the text only is shown, as the status says it (`t` repeats it).
    text_only_why: String,
}

impl Viewer {
    /// A viewer of `name`. `picker` draws pictures (`None`: text only);
    /// `no_child` says why there is no child, when there is none.
    pub(crate) fn new(name: &str, picker: Option<Picker>, no_child: Option<String>) -> Viewer {
        let graphics = picker
            .as_ref()
            .is_some_and(|p| p.protocol_type() != ProtocolType::Halfblocks);
        let font = picker.as_ref().map_or((8, 16), |p| p.font_size());
        let mut v = Viewer {
            name: name.to_string(),
            meta: None,
            page: 0,
            mode: if graphics { Mode::Image } else { Mode::Text },
            fit: Fit::Page,
            scroll: 0,
            text_only: false,
            unopened: None,
            status: String::new(),
            goto: None,
            counts: InputCounts::default(),
            texts: HashMap::new(),
            rasters: Vec::new(),
            failed: BTreeMap::new(),
            asked_info: false,
            pending: None,
            body: Rect::default(),
            font: (font.0.max(1), font.1.max(1)),
            picker,
            proto: None,
            goto_at: None,
            shown: Vec::new(),
            text_only_why: String::new(),
        };
        if let Some(why) = no_child {
            v.fall_back(&why, true);
        }
        v
    }

    /// From now on the pages come from the text importer, because `why`;
    /// `missing` when there is no child, so the status says how to get one.
    pub(crate) fn fall_back(&mut self, why: &str, missing: bool) {
        self.text_only = true;
        self.mode = Mode::Text;
        self.meta = None;
        self.asked_info = false;
        self.pending = None;
        self.texts.clear();
        self.rasters.clear();
        self.failed.clear();
        self.proto = None;
        self.page = 0;
        self.scroll = 0;
        self.text_only_why = if missing {
            format!("Text only: {why}; {HOW_TO_GET_PAGES}")
        } else {
            format!("Text only: {why}")
        };
        self.status = self.text_only_why.clone();
    }

    /// A question is pending.
    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    fn pages(&self) -> usize {
        self.meta.as_ref().map_or(0, |m| m.pages.len())
    }

    // ---- what to ask --------------------------------------------------------------

    /// The raster page `page` would be drawn from in the body as it is.
    fn want(&self, page: usize) -> Option<Want> {
        if self.body.width == 0 || self.body.height == 0 {
            return None;
        }
        let w = u32::from(self.body.width) * u32::from(self.font.0);
        let h = u32::from(self.body.height) * u32::from(self.font.1);
        Some(match self.fit {
            Fit::Page => Want {
                page,
                fit: Fit::Page,
                w,
                h,
            },
            Fit::Width => Want {
                page,
                fit: Fit::Width,
                w,
                h: 0,
            },
        })
    }

    /// Pixels a point for `want`.
    fn scale(&self, want: Want) -> f32 {
        let Some(p) = self.meta.as_ref().and_then(|m| m.pages.get(want.page)) else {
            return 1.;
        };
        let (w, h) = (p.w.max(1.), p.h.max(1.));
        match want.fit {
            Fit::Page => (want.w as f32 / w).min(want.h as f32 / h),
            Fit::Width => want.w as f32 / w,
        }
    }

    /// The next question for the source, taken (it is now pending); `None`
    /// while one is pending or nothing is wanted.
    pub(crate) fn take_next(&mut self) -> Option<Req> {
        if self.pending.is_some() || self.unopened.is_some() {
            return None;
        }
        let (req, want) = self.next_wanted()?;
        self.pending = Some((req.clone(), want));
        Some(req)
    }

    fn next_wanted(&self) -> Option<(Req, Option<Want>)> {
        let Some(meta) = &self.meta else {
            return (!self.asked_info).then_some((Req::Info, None));
        };
        let n = meta.pages.len();
        // The page shown, then the next one.
        for page in [self.page, self.page + 1] {
            if page >= n {
                continue;
            }
            match self.mode {
                Mode::Image if !self.text_only => {
                    let want = self.want(page)?;
                    if self.failed.contains_key(&page) || self.rasters.iter().any(|r| r.0 == want) {
                        continue;
                    }
                    let scale = self.scale(want);
                    return Some((Req::Pixels { page, scale }, Some(want)));
                }
                _ => {
                    if !self.texts.contains_key(&page) {
                        return Some((Req::Text { page }, None));
                    }
                }
            }
        }
        None
    }

    /// Take in the source's answer to the pending question.
    pub(crate) fn on_answer(&mut self, req: Req, ans: Ans) -> Next {
        let want = match self.pending.take() {
            Some((pending, want)) if pending == req => want,
            other => {
                // Not the question asked (an older source's): keep waiting.
                self.pending = other;
                return Next::Nothing;
            }
        };
        if matches!(req, Req::Info) {
            self.asked_info = true;
        }
        // A child that could not be started fails anything but the opening.
        let ans = match (&req, ans) {
            (Req::Info, ans) => ans,
            (_, Ans::NoChild(why)) => Ans::Failed(why),
            (_, ans) => ans,
        };
        match (req, ans) {
            (Req::Info, Ans::Info(m)) => {
                self.page = self.page.min(m.pages.len().saturating_sub(1));
                self.meta = Some(m);
            }
            (Req::Info, Ans::Locked) => {
                self.unopened = Some(LOCKED.into());
                self.status.clear();
            }
            (Req::Info, Ans::NoChild(why)) if !self.text_only => {
                return Next::TextOnly { why, missing: true };
            }
            // The child died or ran out of time on the file: it is not read
            // again in docxy's own process.
            (Req::Info, Ans::Failed(why))
                if self.text_only || why == child::DIED || why == child::TOO_LONG =>
            {
                self.unopened = Some(format!("This PDF can't be opened: {why}"));
            }
            (Req::Info, Ans::Failed(why)) => {
                return Next::TextOnly {
                    why,
                    missing: false,
                };
            }
            (Req::Pixels { .. }, Ans::Pixels(r)) => {
                if let Some(want) = want {
                    if self.rasters.len() >= RASTERS_KEPT {
                        // The least recently answered, but never the page shown.
                        if let Some(i) = self.rasters.iter().position(|r| r.0.page != self.page) {
                            self.rasters.remove(i);
                        } else {
                            self.rasters.remove(0);
                        }
                    }
                    self.rasters.push((want, r));
                }
            }
            (Req::Pixels { page, .. }, Ans::Failed(why)) => {
                self.failed.insert(page, why);
            }
            (Req::Text { page }, Ans::Text(t)) => {
                self.texts.insert(page, Ok(clean_text(&t)));
            }
            (Req::Text { page }, Ans::Failed(why)) => {
                self.texts.insert(page, Err(why));
            }
            _ => {}
        }
        Next::Nothing
    }

    // ---- keys ----------------------------------------------------------------------

    /// Handle a key; `true` to quit.
    pub(crate) fn on_key(&mut self, k: KeyEvent) -> bool {
        if k.kind != KeyEventKind::Press {
            return false;
        }
        if let Some(digits) = self.goto.as_mut() {
            match k.code {
                KeyCode::Char(c) if c.is_ascii_digit() && digits.len() < 9 => digits.push(c),
                KeyCode::Backspace => {
                    digits.pop();
                }
                KeyCode::Enter => {
                    let digits = self.goto.take().unwrap_or_default();
                    self.go_to_typed(&digits);
                }
                KeyCode::Esc => self.goto = None,
                _ => {}
            }
            return false;
        }
        match k.code {
            // q, Esc and Ctrl+Q.
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::PageDown | KeyCode::Right | KeyCode::Char(' ' | 'n') => self.turn(1),
            KeyCode::PageUp | KeyCode::Left | KeyCode::Char('p') => self.turn(-1),
            KeyCode::Home => self.go_to(0),
            KeyCode::End => self.go_to(self.pages().saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Char('g') => {
                if self.pages() > 1 {
                    self.goto = Some(String::new());
                }
            }
            KeyCode::Char('t') => self.toggle_mode(),
            KeyCode::Char('w') => {
                self.fit = match self.fit {
                    Fit::Page => Fit::Width,
                    Fit::Width => Fit::Page,
                };
                self.scroll = 0;
            }
            _ => {}
        }
        false
    }

    fn turn(&mut self, by: isize) {
        let to = self.page.saturating_add_signed(by);
        self.go_to(to);
    }

    fn go_to(&mut self, page: usize) {
        let last = self.pages().saturating_sub(1);
        let page = page.min(last);
        if page != self.page {
            self.page = page;
            self.scroll = 0;
        }
    }

    fn go_to_typed(&mut self, digits: &str) {
        match digits.parse::<usize>() {
            Ok(n) if (1..=self.pages()).contains(&n) => {
                self.go_to(n - 1);
                self.status.clear();
            }
            _ if digits.is_empty() => {}
            _ => self.status = format!("There is no page {digits}: the PDF has {}", self.pages()),
        }
    }

    fn toggle_mode(&mut self) {
        if self.text_only {
            self.status = self.text_only_why.clone();
            return;
        }
        self.mode = match self.mode {
            Mode::Image => Mode::Text,
            Mode::Text => Mode::Image,
        };
        self.scroll = 0;
    }

    // ---- drawing ---------------------------------------------------------------------

    pub(crate) fn draw(&mut self, f: &mut Frame) {
        self.counts.frames += 1;
        let area = f.area();
        if area.height < 3 || area.width < 4 {
            return;
        }
        let header = Rect { height: 1, ..area };
        let body = Rect {
            y: area.y + 1,
            height: area.height - 2,
            ..area
        };
        let footer = Rect {
            y: area.y + area.height - 1,
            height: 1,
            ..area
        };
        if body != self.body {
            self.body = body;
            self.proto = None;
        }
        let bar = Style::default().add_modifier(Modifier::REVERSED);
        f.render_widget(
            Paragraph::new(clean_line(&self.header())).style(bar),
            header,
        );
        self.draw_body(f, body);
        self.goto_at = None;
        let foot = match &self.goto {
            Some(digits) => {
                let label = " Go to page: ";
                self.goto_at = Some((footer.x + label.len() as u16, footer.y));
                f.set_cursor_position((footer.x + (label.len() + digits.len()) as u16, footer.y));
                format!("{label}{digits}")
            }
            None if !self.status.is_empty() => format!(" {}", clean_line(&self.status)),
            None => format!(" {}", self.hint()),
        };
        f.render_widget(Paragraph::new(foot).style(bar), footer);
    }

    fn header(&self) -> String {
        let at = match &self.meta {
            _ if self.text_only => "text only".to_string(),
            Some(m) => {
                let n = self.page + 1;
                let label = m.pages.get(self.page).map_or("", |p| p.label.as_str());
                if label.is_empty() || label == n.to_string() {
                    format!("Page {n} of {}", m.pages.len())
                } else {
                    format!("Page {label} ({n} of {})", m.pages.len())
                }
            }
            None => "opening".to_string(),
        };
        let how = match (self.mode, self.fit) {
            _ if self.text_only => "",
            (Mode::Text, _) => " · text",
            (Mode::Image, Fit::Page) => " · fit page",
            (Mode::Image, Fit::Width) => " · fit width",
        };
        format!(" {} · {at}{how}", self.name)
    }

    fn hint(&self) -> &'static str {
        if self.text_only {
            "↑↓ scroll · q quit"
        } else {
            "PgDn/PgUp page · Home/End · g go to · t text/page · w fit · ↑↓ scroll · q quit"
        }
    }

    fn draw_body(&mut self, f: &mut Frame, body: Rect) {
        self.shown.clear();
        let text: Option<String> = if let Some(why) = &self.unopened {
            Some(why.clone())
        } else if self.meta.is_none() {
            Some(if self.text_only {
                format!("Reading the text of {}…", self.name)
            } else {
                format!("Opening {}…", self.name)
            })
        } else if self.mode == Mode::Text || self.text_only {
            Some(match self.texts.get(&self.page) {
                Some(Ok(t)) => t.clone(),
                Some(Err(why)) => format!("The text of this page can't be read: {why}"),
                None => format!("Reading page {}…", self.page + 1),
            })
        } else {
            self.failed
                .get(&self.page)
                .map(|why| format!("This page can't be drawn: {why}"))
        };
        match text {
            Some(t) => {
                let lines = wrap(&clean_text(&t), usize::from(body.width));
                let max = lines.len().saturating_sub(usize::from(body.height));
                self.scroll = self.scroll.min(max);
                let shown: Vec<String> = lines
                    .into_iter()
                    .skip(self.scroll)
                    .take(usize::from(body.height))
                    .collect();
                let para: Vec<Line> = shown.iter().map(|l| Line::from(l.as_str())).collect();
                f.render_widget(Paragraph::new(para), body);
                self.shown = shown;
            }
            None => self.draw_page(f, body),
        }
    }

    fn draw_page(&mut self, f: &mut Frame, body: Rect) {
        let raster = self
            .want(self.page)
            .and_then(|want| self.rasters.iter().find(|r| r.0 == want))
            .map(|(w, r)| (*w, r.clone()));
        let Some((want, r)) = raster else {
            let wait = format!("Drawing page {}…", self.page + 1);
            f.render_widget(Paragraph::new(Line::from(Span::raw(wait))), body);
            return;
        };
        let (fw, fh) = (u32::from(self.font.0), u32::from(self.font.1));
        let rows = r.height.div_ceil(fh) as usize;
        let max = rows.saturating_sub(usize::from(body.height));
        self.scroll = if want.fit == Fit::Width {
            self.scroll.min(max)
        } else {
            0
        };
        let top = self.scroll as u32 * fh;
        let crop_h = r
            .height
            .saturating_sub(top)
            .min(u32::from(body.height) * fh);
        let cols = (r.width.div_ceil(fw) as u16).min(body.width);
        let rows = (crop_h.div_ceil(fh) as u16).min(body.height).max(1);
        let rect = Rect {
            x: body.x + (body.width - cols) / 2,
            y: body.y,
            width: cols.max(1),
            height: rows,
        };
        let key = (want, self.scroll, rect);
        if self.proto.as_ref().is_none_or(|(k, _)| *k != key) {
            self.proto = None;
            let Some(picker) = self.picker.as_ref() else {
                return;
            };
            let Some(img) = image::RgbaImage::from_raw(r.width, r.height, r.rgba) else {
                return;
            };
            let img = image::DynamicImage::ImageRgba8(img).crop_imm(0, top, r.width, crop_h.max(1));
            let size = Rect {
                x: 0,
                y: 0,
                width: rect.width,
                height: rect.height,
            };
            if let Ok(p) = picker.new_protocol(img, size, Resize::Fit(None)) {
                self.proto = Some((key, p));
            }
        }
        if let Some((_, p)) = &self.proto {
            f.render_widget(Image::new(p), rect);
        }
    }

    // ---- control verbs ---------------------------------------------------------------

    /// `field-read` and `pdf-read`; anything else is refused.
    pub(crate) fn dispatch(&mut self, verb: &str, args: &Json) -> Result<Json, String> {
        match verb {
            "field-read" => {
                let counts = self.counts;
                let field = match &self.goto {
                    Some(d) => Field::with_caret(d, true, d.chars().count()).at(self.goto_at),
                    None => Field::closed(),
                };
                ctlcore::field::reply(args, INPUTS, counts, false, |_| field)
            }
            "pdf-read" => Ok(self.read()),
            _ => Err(format!(
                "unknown verb '{verb}': docxy's PDF viewer answers field-read and pdf-read"
            )),
        }
    }

    pub(crate) fn read(&self) -> Json {
        let num = |n: usize| Json::Num(n as f64);
        let s = |t: &str| Json::Str(t.to_string());
        let failed = self
            .failed
            .iter()
            .map(|(p, why)| (format!("{}", p + 1), Json::Str(why.clone())))
            .collect::<Vec<_>>();
        Json::obj(vec![
            ("name", s(&self.name)),
            ("pages", num(self.pages())),
            ("page", num(self.page + 1)),
            (
                "label",
                self.meta
                    .as_ref()
                    .and_then(|m| m.pages.get(self.page))
                    .map_or(Json::Null, |p| s(&p.label)),
            ),
            (
                "mode",
                s(match self.mode {
                    Mode::Image => "image",
                    Mode::Text => "text",
                }),
            ),
            (
                "fit",
                s(match self.fit {
                    Fit::Page => "page",
                    Fit::Width => "width",
                }),
            ),
            ("scroll", num(self.scroll)),
            (
                "source",
                s(if self.text_only { "text-only" } else { "child" }),
            ),
            ("status", s(&self.status)),
            ("unopened", self.unopened.as_deref().map_or(Json::Null, s)),
            ("failed", Json::Obj(failed)),
            (
                "drawn",
                Json::Bool(
                    self.want(self.page)
                        .is_some_and(|w| self.rasters.iter().any(|r| r.0 == w)),
                ),
            ),
            ("busy", Json::Bool(self.pending.is_some())),
            (
                "outline",
                Json::Arr(
                    self.meta
                        .as_ref()
                        .map(|m| {
                            m.outline
                                .iter()
                                .map(|(t, p, d)| {
                                    Json::obj(vec![
                                        ("title", s(t)),
                                        ("page", p.map_or(Json::Null, |p| num(p + 1))),
                                        ("depth", num(*d)),
                                    ])
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
            ),
            (
                "lines",
                Json::Arr(self.shown.iter().map(|l| s(l)).collect()),
            ),
        ])
    }
}

/// `text` as it may be drawn in cells: a PDF's text (or its file name, or
/// a reason the child gives) is the file's, and a terminal acts on control
/// characters. C0 and C1 controls (but `\n`) and DEL become U+FFFD, so no
/// ESC, BEL, CSI or OSC reaches a cell; `\r\n` and a lone `\r` are line
/// breaks; a tab is spaces to the next multiple of 8 columns; bidi controls
/// and invisible format characters that would move or hide what the cells
/// show are dropped (ZWJ and ZWNJ, which join, are kept).
pub(crate) fn clean_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut col = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                out.push('\n');
                col = 0;
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
                col = 0;
            }
            '\t' => {
                let n = 8 - col % 8;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            c if c.is_control() => {
                out.push('\u{FFFD}');
                col += 1;
            }
            '\u{061C}'
            | '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}' => {}
            c => {
                out.push(c);
                col += c.width().unwrap_or(0);
            }
        }
    }
    out
}

/// [`clean_text`] on one line: its line breaks are spaces.
pub(crate) fn clean_line(text: &str) -> String {
    clean_text(text).replace('\n', " ")
}

/// `text`'s lines wrapped to `width` columns by display width (a wide
/// character takes two): at spaces, and inside a word longer than a line.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let w = |c: char| c.width().unwrap_or(0);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        let mut cur = String::new();
        let mut cols = 0;
        for word in line.split_inclusive(' ') {
            let word_cols: usize = word.trim_end_matches(' ').chars().map(w).sum();
            if cols > 0 && cols + word_cols > width {
                out.push(cur.trim_end().to_string());
                cur.clear();
                cols = 0;
            }
            for c in word.chars() {
                let cw = w(c);
                if c != ' ' && cols > 0 && cols + cw > width {
                    out.push(std::mem::take(&mut cur));
                    cols = 0;
                }
                cur.push(c);
                cols += cw;
            }
        }
        out.push(cur.trim_end().to_string());
    }
    out
}

// ---- the engine and the loop -------------------------------------------------------------

/// The engine thread's name: the panic hook leaves its panics to it.
const ENGINE_THREAD: &str = "docxy-pdf-engine";

/// A source answering on a thread of its own: `ask` never waits. A panic
/// in the source answers [`child::DIED`] (the viewer is never left waiting);
/// dropping the engine, or [`Engine::kill`], ends the source's child.
pub(crate) struct Engine {
    tx: mpsc::Sender<Req>,
    killer: Option<Killer>,
}

impl Engine {
    pub(crate) fn start(
        mut source: Box<dyn Source>,
        answer: impl Fn(Req, Ans) -> bool + Send + 'static,
    ) -> Engine {
        let killer = source.killer();
        let (tx, rx) = mpsc::channel::<Req>();
        let _ = std::thread::Builder::new()
            .name(ENGINE_THREAD.into())
            .spawn(move || {
                for req in rx {
                    let ans =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source.ask(&req)))
                            .unwrap_or_else(|_| Ans::Failed(child::DIED.into()));
                    if !answer(req, ans) {
                        return;
                    }
                }
            });
        Engine { tx, killer }
    }

    pub(crate) fn ask(&self, req: Req) {
        let _ = self.tx.send(req);
    }

    /// End the source's child now, even mid-question (a quit): the engine
    /// thread's wait ends with the child's output.
    pub(crate) fn kill(&self) {
        if let Some(k) = &self.killer {
            k.kill();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Puts the terminal back however the viewer ends.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        use ratatui::crossterm::execute;
        use ratatui::crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    }
}

/// `docxy <file.pdf>`: the viewer, until q.
pub(crate) fn run(path: &str) -> std::io::Result<()> {
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{self, Event};
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::{EnterAlternateScreen, SetTitle, enable_raw_mode};

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // The engine catches its own (it answers DIED): neither the screen
        // nor the terminal's state is touched.
        if std::thread::current().name() == Some(ENGINE_THREAD) {
            return;
        }
        drop(Restore);
        default_hook(info);
    }));
    enable_raw_mode()?;
    let restore = Restore;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = ratatui::Terminal::new(CrosstermBackend::new(stdout))?;

    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16)));
    let file = PathBuf::from(path);
    let name = file
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
    let _ = execute!(std::io::stdout(), SetTitle(format!("{name} - docxy")));
    let (source, no_child): (Box<dyn Source>, Option<String>) = match child::find_child() {
        Ok(exe) => (Box::new(Child::new(exe, file.clone())), None),
        Err(why) => (Box::new(TextOnly::new(file.clone())), Some(why)),
    };
    let mut viewer = Viewer::new(&name, Some(picker), no_child);

    let (ctl_server, ctl_rx) = match crate::control::control_dir() {
        Some(dir) => match ctlcore::serve(&dir, &crate::control::instance_name()) {
            Ok((srv, rx)) => (Some(srv), Some(rx)),
            Err(_) => (None, None),
        },
        None => (None, None),
    };

    enum Msg {
        Term(Event),
        Ctl(ctlcore::Request),
        Pdf(Req, Ans),
    }
    let (tx, rx) = mpsc::channel::<Msg>();
    {
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("docxy-input".into())
            .spawn(move || {
                while let Ok(ev) = event::read() {
                    if tx.send(Msg::Term(ev)).is_err() {
                        break;
                    }
                }
            });
    }
    if let Some(ctl_rx) = ctl_rx {
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("docxy-ctl".into())
            .spawn(move || {
                for req in ctl_rx {
                    if tx.send(Msg::Ctl(req)).is_err() {
                        break;
                    }
                }
            });
    }
    let engine_for = |source: Box<dyn Source>, tx: &mpsc::Sender<Msg>| {
        let tx = tx.clone();
        Engine::start(source, move |req, ans| tx.send(Msg::Pdf(req, ans)).is_ok())
    };
    let mut engine = engine_for(source, &tx);

    // `pdf-read {"until_idle":true}` replies once nothing is pending, or at
    // its deadline: a harness reads what an answer changed without polling.
    let mut waiting: Vec<(ctlcore::Request, std::time::Instant)> = Vec::new();
    let result = loop {
        if let Err(e) = terminal.draw(|f| viewer.draw(f)) {
            break Err(e);
        }
        // The body is known once drawn: ask what it needs.
        if let Some(req) = viewer.take_next() {
            engine.ask(req);
        }
        let now = std::time::Instant::now();
        let (ready, still): (Vec<_>, Vec<_>) = waiting
            .drain(..)
            .partition(|(_, by)| !viewer.busy() || now >= *by);
        waiting = still;
        for (req, _) in ready {
            req.reply_ok(viewer.read());
        }
        let first = match waiting.iter().map(|w| w.1).min() {
            Some(by) => match rx.recv_timeout(by.saturating_duration_since(now)) {
                Ok(m) => Ok(Some(m)),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(()),
            },
            None => rx.recv().map(Some).map_err(|_| ()),
        };
        let mut next = match first {
            Ok(m) => m,
            Err(()) => break Ok(()),
        };
        let mut quit = false;
        while let Some(msg) = next.take() {
            match msg {
                Msg::Term(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                    viewer.counts.keys += 1;
                    quit = viewer.on_key(k);
                }
                Msg::Term(_) => {}
                Msg::Ctl(req)
                    if req.verb == "pdf-read"
                        && req.args.get("until_idle").and_then(Json::as_bool) == Some(true)
                        && viewer.busy() =>
                {
                    waiting.push((req, std::time::Instant::now() + UNTIL_IDLE));
                }
                Msg::Ctl(req) => match viewer.dispatch(&req.verb, &req.args) {
                    Ok(j) => req.reply_ok(j),
                    Err(e) => req.reply_err(e),
                },
                Msg::Pdf(req, ans) => {
                    if let Next::TextOnly { why, missing } = viewer.on_answer(req, ans) {
                        viewer.fall_back(&why, missing);
                        engine = engine_for(Box::new(TextOnly::new(file.clone())), &tx);
                    }
                }
            }
            if quit {
                break;
            }
            next = rx.try_recv().ok();
        }
        if quit {
            break Ok(());
        }
    };
    // A quit mid-question ends the child now, not at its watchdog.
    engine.kill();
    drop(ctl_server);
    drop(restore);
    terminal.show_cursor()?;
    result
}
