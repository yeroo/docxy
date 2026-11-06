//! Document → e-mail-safe HTML (#1948): the body a mail client sends for a
//! message composed as a [`Document`], read back by
//! [`crate::import::html::import_html_with`].
//!
//! Mail clients strip `<style>` blocks and ignore classes, so every look is
//! an inline `style` attribute (or a presentational attribute: `<ol
//! type start>`, `<img width height>`, `<td colspan rowspan>`), and the page
//! has neither a `<style>` element nor a `class`. Formatting a paragraph or
//! run takes from its styles is resolved and written as direct formatting,
//! as the Rich Text writer does.
//!
//! Written: runs (bold, italic, underline, strike, superscript, subscript,
//! caps, small caps, font, size, colour, highlight), paragraphs (headings
//! 1 to 6, alignment, direction, left / right / first-line indent, space
//! before and after, line spacing), lists as nested `<ul>` / `<ol>` by
//! level (numbering that goes on after a plain paragraph as `start`),
//! hyperlinks with a safe target (`http`, `https`, `ftp`, `mailto`, or the
//! document's own `#anchor`), inline pictures as `<img src="cid:…">` whose
//! media come back beside the HTML ([`EmailMedia`]) to attach inline,
//! tables (cells, column and row spans; no borders, widths or shading),
//! tabs, breaks, fields, equations and text boxes as their text, and a
//! note reference as its number. Tracked changes are their final view, and
//! hidden text (direct or from a style) is never written.
//!
//! Not written: floating pictures, headers and footers, note bodies,
//! comments, a paragraph style's spacing and indent (only the paragraph's
//! own), lists that only a paragraph style numbers, and content the model
//! keeps only as raw XML.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::export_context::{
    ExportContext, effective_run, final_view, marker_format, text_box_paragraphs,
};
use crate::highlight::highlight_rgb;
use crate::load::raw_run_props;
use crate::model::{
    Align, Block, Cell, Document, Hyperlink, Inline, Paragraph, RevisionKind, RunProps, Table,
    VMerge, VertAlign,
};
use crate::numbering::{NumFmt, Numbering};
use crate::package::Package;
use crate::styles::StyleSheet;
use crate::table::GridMap;

/// The part pictures' relationships belong to.
const DOCUMENT_PART: &str = "word/document.xml";

/// A message body: the HTML, and the pictures its `cid:` URLs name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EmailHtml {
    /// A whole UTF-8 document (`<!DOCTYPE html>` … `</html>`).
    pub html: String,
    /// Each picture once, in the order the HTML first shows it.
    pub media: Vec<EmailMedia>,
}

/// One picture to attach inline: a `Content-ID` the HTML's `cid:` URLs
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMedia {
    /// The id as in the `cid:` URL: no `cid:`, no angle brackets.
    pub content_id: String,
    pub bytes: Vec<u8>,
    /// The media part's own name (`image1.png`).
    pub file_name: String,
    /// The package's content type for the part, else the one its extension
    /// says, else `application/octet-stream`.
    pub content_type: String,
}

/// `doc` as an e-mail body, its styles, lists and pictures as `pkg` holds
/// them. With no package, the styles and lists are a new Markdown
/// package's and pictures are their Alt Text.
pub fn to_email_html(doc: &Document, pkg: Option<&Package>) -> EmailHtml {
    let ctx = ExportContext::for_package(pkg);
    let doc = &final_view(doc);
    // On the final view: the keys are its paths.
    let markers = ctx.markers(doc);
    let mut w = Writer {
        styles: &ctx.styles,
        numbering: &ctx.numbering,
        markers: &markers,
        pkg,
        out: String::new(),
        media: Vec::new(),
        media_index: HashMap::new(),
        lists: Vec::new(),
        counters: HashMap::new(),
        text: TextState::default(),
        in_heading: false,
        shown: 0,
    };
    w.out
        .push_str("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n</head>\n<body>\n");
    let mut path = Vec::new();
    w.blocks(&doc.body, &mut path);
    w.close_lists(0);
    w.out.push_str("</body>\n</html>\n");
    EmailHtml {
        html: w.out,
        media: w.media,
    }
}

/// What a list level is drawn as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListKind {
    Bullet,
    /// An `<ol>` of this `type`.
    Ordered(&'static str),
}

/// A list open at one level.
struct OpenList {
    num_id: i32,
    kind: ListKind,
    /// Its `<li>` is open (a deeper list goes inside it).
    item_open: bool,
    /// The number a browser shows on its next `<li>`.
    next: i32,
}

/// Where the text of the paragraph being written stands, so runs of spaces
/// and the paragraph's edge spaces survive a browser's white-space collapse.
#[derive(Clone, Default)]
struct TextState {
    /// Nothing shown yet on this line, or the last character was a space: a
    /// space now would collapse.
    after_space: bool,
    /// Where in the output the last plain space is, while it is the last
    /// thing shown: at the paragraph's end it becomes `&nbsp;`.
    trailing_space: Option<usize>,
    /// Something visible was written.
    shown: bool,
}

struct Writer<'a> {
    styles: &'a StyleSheet,
    numbering: &'a Numbering,
    markers: &'a HashMap<Vec<usize>, String>,
    pkg: Option<&'a Package>,
    out: String,
    media: Vec<EmailMedia>,
    /// Media part name → its index in `media`.
    media_index: HashMap<String, usize>,
    /// The open lists, one per level from 0.
    lists: Vec<OpenList>,
    /// The numbering's counters (`Numbering::step`), in document order.
    counters: HashMap<i32, Vec<i32>>,
    text: TextState,
    /// The paragraph being written is an `<hN>`, which browsers make bold.
    in_heading: bool,
    /// How many visible things have been written: a link that adds none
    /// is taken back.
    shown: u64,
}

impl Writer<'_> {
    fn blocks(&mut self, blocks: &[Block], path: &mut Vec<usize>) {
        for (i, block) in blocks.iter().enumerate() {
            path.push(i);
            match block {
                Block::Paragraph(p) => self.paragraph(p, path),
                Block::Table(t) => {
                    self.close_lists(0);
                    self.table(t, path);
                }
                Block::Raw(_) | Block::SectionProperties(_) => {}
            }
            path.pop();
        }
    }

    fn paragraph(&mut self, p: &Paragraph, path: &[usize]) {
        let pstyle = p.props.style_id.as_deref();
        // Every numbered paragraph counts, headings included, as the
        // markers count them.
        let ordinal = p
            .props
            .num_id
            .and_then(|n| self.numbering.step(n, p.props.ilvl, &mut self.counters));
        let heading = p.props.heading_level;
        let listed = p.props.num_id.filter(|&n| n != 0 && heading.is_none());
        let dir = if self.styles.paragraph_rtl_override(pstyle, &p.props) == Some(true) {
            " dir=\"rtl\""
        } else {
            ""
        };
        if let Some(num_id) = listed {
            let ilvl = p.props.ilvl.clamp(0, 8) as usize;
            let kind = self.list_kind(num_id, ilvl, path);
            let value = match self.open_item(num_id, ilvl, kind, ordinal) {
                Some(n) => format!(" value=\"{n}\""),
                None => String::new(),
            };
            let style = self.paragraph_css(p, true);
            let _ = write!(self.out, "<li{dir}{value} style=\"{}\">", attr(&style));
            self.content(p);
            // Closed by the next item, or when the list ends.
            return;
        }
        self.close_lists(0);
        let tag = match heading {
            Some(level @ 1..=6) => format!("h{level}"),
            _ => "p".to_string(),
        };
        self.in_heading = tag != "p";
        let style = self.paragraph_css(p, false);
        let _ = write!(self.out, "<{tag}{dir} style=\"{}\">", attr(&style));
        self.content(p);
        let _ = writeln!(self.out, "</{tag}>");
        self.in_heading = false;
    }

    /// A paragraph's inlines, `&nbsp;` when nothing shows (a mail client
    /// drops an empty paragraph's height).
    fn content(&mut self, p: &Paragraph) {
        self.text = TextState {
            after_space: true,
            ..TextState::default()
        };
        self.inlines(&p.content, p.props.style_id.as_deref());
        if let Some(at) = self.text.trailing_space.take() {
            self.out.replace_range(at..at + 1, "&nbsp;");
        }
        if !self.text.shown {
            self.out.push_str("&nbsp;");
        }
    }

    /// The paragraph's look as CSS. A list item's left and first-line
    /// indent are its level's (the mail client's list padding), so they are
    /// not written.
    fn paragraph_css(&self, p: &Paragraph, item: bool) -> String {
        let props = &p.props;
        let pstyle = props.style_id.as_deref();
        let tw = |t: Option<i32>| pt(t.unwrap_or(0).max(0));
        let mut css = if item {
            format!(
                "margin-top:{}pt;margin-right:{}pt;margin-bottom:{}pt",
                tw(props.spacing.before),
                pt(props.indent_right),
                tw(props.spacing.after)
            )
        } else {
            // Every side, 0 when unset, so a browser's own `p` margin never
            // shows.
            format!(
                "margin:{}pt {}pt {}pt {}pt",
                tw(props.spacing.before),
                pt(props.indent_right),
                tw(props.spacing.after),
                pt(props.indent)
            )
        };
        if !item && props.first_line != 0 {
            let _ = write!(css, ";text-indent:{}pt", pt(props.first_line));
        }
        let rtl = self.styles.paragraph_rtl_override(pstyle, props) == Some(true);
        // A right-to-left paragraph's start is its right edge, as the
        // renderer aligns it.
        let align = match self.styles.effective_align(pstyle, props) {
            Align::Left if rtl => "right",
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
            Align::Justify => "justify",
        };
        let _ = write!(css, ";text-align:{align}");
        if let Some(line) = props.spacing.line.filter(|&l| l > 0) {
            match props.spacing.line_rule.as_deref() {
                Some("exact") => {
                    let _ = write!(
                        css,
                        ";line-height:{}pt;mso-line-height-rule:exactly",
                        pt(line)
                    );
                }
                Some("atLeast") => {
                    let _ = write!(css, ";line-height:{}pt", pt(line));
                }
                _ => {
                    let _ = write!(css, ";line-height:{}", num(f64::from(line) / 240.0, 4));
                }
            }
        }
        css
    }

    /// What list level `ilvl` of `num_id` is: its definition's format, else
    /// what the paragraph's marker says.
    fn list_kind(&self, num_id: i32, ilvl: usize, path: &[usize]) -> ListKind {
        if let Some(def) = self.numbering.level(num_id, ilvl as i32) {
            return ListKind::from_fmt(def.format);
        }
        match self.markers.get(path) {
            Some(m) => marker_format(m).map_or(ListKind::Bullet, ListKind::from_fmt),
            None => ListKind::Bullet,
        }
    }

    /// Open the `<li>` of a list item at `ilvl`: close deeper lists, close
    /// or reopen the list at its level, and open the lists down to it. The
    /// item's `value` when its number (`ordinal`) is not the one its `<ol>`
    /// shows next: a list opened between levels, then reused, has no
    /// `start` of its own.
    fn open_item(
        &mut self,
        num_id: i32,
        ilvl: usize,
        kind: ListKind,
        ordinal: Option<i32>,
    ) -> Option<i32> {
        self.close_lists(ilvl + 1);
        if let Some(top) = self.lists.get(ilvl) {
            if top.num_id != num_id || top.kind != kind {
                self.close_lists(ilvl);
            } else if top.item_open {
                self.out.push_str("</li>\n");
                self.lists[ilvl].item_open = false;
            }
        }
        // A jump of more than one level opens the levels between directly:
        // HTML lets a list hold a list, and a reader counts its depth.
        while self.lists.len() <= ilvl {
            let own = self.lists.len() == ilvl;
            let start = ordinal.filter(|&n| own && n != 1);
            self.open_list(kind, start);
            self.lists.push(OpenList {
                num_id,
                kind,
                item_open: false,
                next: start.unwrap_or(1),
            });
        }
        let list = &mut self.lists[ilvl];
        list.item_open = true;
        let shown = ordinal.unwrap_or(list.next);
        let value = (matches!(kind, ListKind::Ordered(_)) && shown != list.next).then_some(shown);
        list.next = shown.saturating_add(1);
        value
    }

    fn open_list(&mut self, kind: ListKind, start: Option<i32>) {
        const STYLE: &str = "style=\"margin-top:0;margin-bottom:0\"";
        match kind {
            ListKind::Bullet => {
                let _ = writeln!(self.out, "<ul {STYLE}>");
            }
            ListKind::Ordered(ty) => {
                let _ = write!(self.out, "<ol type=\"{ty}\"");
                if let Some(n) = start {
                    let _ = write!(self.out, " start=\"{n}\"");
                }
                let _ = writeln!(self.out, " {STYLE}>");
            }
        }
    }

    /// Close every open list at level `depth` and deeper.
    fn close_lists(&mut self, depth: usize) {
        while self.lists.len() > depth {
            let l = self.lists.pop().expect("a list is open");
            if l.item_open {
                self.out.push_str("</li>\n");
            }
            self.out.push_str(match l.kind {
                ListKind::Bullet => "</ul>\n",
                ListKind::Ordered(_) => "</ol>\n",
            });
        }
    }

    fn table(&mut self, t: &Table, path: &mut Vec<usize>) {
        let map = GridMap::of(t);
        self.out.push_str(
            "<table cellspacing=\"0\" cellpadding=\"0\" style=\"border-collapse:collapse\">\n",
        );
        for (ri, row) in t.rows.iter().enumerate() {
            self.out.push_str("<tr>\n");
            for (ci, cell) in row.cells.iter().enumerate() {
                if cell.v_merge == VMerge::Continue {
                    continue;
                }
                let (start, span) = map.rows[ri].cells[ci];
                let rows = if cell.v_merge == VMerge::Restart {
                    1 + continued_rows(t, &map, ri, start)
                } else {
                    1
                };
                self.out
                    .push_str("<td style=\"vertical-align:top;padding:0 5.4pt\"");
                if span > 1 {
                    let _ = write!(self.out, " colspan=\"{span}\"");
                }
                if rows > 1 {
                    let _ = write!(self.out, " rowspan=\"{rows}\"");
                }
                self.out.push_str(">\n");
                path.push(ri);
                path.push(ci);
                self.cell(cell, path);
                path.pop();
                path.pop();
                self.out.push_str("</td>\n");
            }
            self.out.push_str("</tr>\n");
        }
        self.out.push_str("</table>\n");
    }

    fn cell(&mut self, cell: &Cell, path: &mut Vec<usize>) {
        let before = self.out.len();
        self.blocks(&cell.blocks, path);
        self.close_lists(0);
        if self.out.len() == before {
            self.out.push_str("&nbsp;");
        }
    }

    fn inlines(&mut self, content: &[Inline], pstyle: Option<&str>) {
        for inline in content {
            match inline {
                Inline::Run(r) => self.run(&r.text, &r.props, pstyle),
                Inline::Hyperlink(h) => self.hyperlink(h, pstyle),
                Inline::Break(_, props) => {
                    if !self.hidden(props, pstyle) {
                        self.line_break();
                    }
                }
                Inline::Tab(props) => {
                    if !self.hidden(props, pstyle) {
                        // Word's tab: browsers keep the tab, and the HTML
                        // import reads the span as one.
                        let css = self.run_css(props, pstyle);
                        let _ = write!(
                            self.out,
                            "<span style=\"mso-tab-count:1;white-space:pre{}{}\">\t</span>",
                            if css.is_empty() { "" } else { ";" },
                            attr(&css)
                        );
                        self.shown_solid();
                    }
                }
                // An object's text in its run's own formatting, which can
                // hide it. An OMML equation has no run of its own: only its
                // paragraph's style can hide it.
                Inline::SmartArt { raw, text } => {
                    self.run(&text.join("\n"), &raw_run_props(raw), pstyle)
                }
                Inline::Chart { raw, chart } => self.run(
                    chart.title.as_deref().unwrap_or(""),
                    &raw_run_props(raw),
                    pstyle,
                ),
                Inline::Equation { raw, text, .. } => self.run(text, &raw_run_props(raw), pstyle),
                Inline::Field { raw, text } => {
                    self.run(text, &crate::load::field_result_props(raw), pstyle)
                }
                Inline::TextBox { raw, .. } if self.hidden(&raw_run_props(raw), pstyle) => {}
                Inline::TextBox { blocks, .. } => {
                    let mut first = true;
                    for p in text_box_paragraphs(blocks) {
                        if !std::mem::take(&mut first) {
                            self.line_break();
                        }
                        self.inlines(&p.content, p.props.style_id.as_deref());
                    }
                }
                Inline::Revision {
                    kind: RevisionKind::Insert,
                    content,
                    ..
                } => self.inlines(content, pstyle),
                Inline::Revision {
                    kind: RevisionKind::Delete,
                    ..
                } => {}
                Inline::UnsupportedRevision { .. } => {}
                Inline::FootnoteRef { raw, .. } if self.hidden(&raw_run_props(raw), pstyle) => {}
                Inline::FootnoteRef { id, .. } => {
                    let _ = write!(self.out, "<sup>{id}</sup>");
                    self.shown_solid();
                }
                Inline::Raw(raw) => {
                    // A hidden picture is neither shown nor attached.
                    if let Some(rid) = crate::image::inline_embed(raw) {
                        if !self.hidden(&raw_run_props(raw), pstyle) {
                            self.picture(raw, &rid);
                        }
                    }
                }
            }
        }
    }

    /// A link: `<a href>` round its content, or its content alone when its
    /// target is not safe. A link that shows nothing (its text hidden or
    /// deleted) writes nothing, so its URL never leaves in the message.
    fn hyperlink(&mut self, h: &Hyperlink, pstyle: Option<&str>) {
        let Some(href) = link_href(h) else {
            self.link_content(h, pstyle);
            return;
        };
        let (mark, text, shown) = (self.out.len(), self.text.clone(), self.shown);
        let _ = write!(self.out, "<a href=\"{}\">", attr(&href));
        self.link_content(h, pstyle);
        if self.shown == shown {
            // Every index `text` holds is before `mark`.
            self.out.truncate(mark);
            self.text = text;
            return;
        }
        self.out.push_str("</a>");
    }

    fn link_content(&mut self, h: &Hyperlink, pstyle: Option<&str>) {
        for r in &h.runs {
            self.run(&r.text, &r.props, pstyle);
        }
        self.inlines(&h.content, pstyle);
    }

    /// Whether a run is hidden text, by its direct formatting or a style:
    /// hidden text never leaves in a sent message.
    fn hidden(&self, props: &RunProps, pstyle: Option<&str>) -> bool {
        self.styles
            .effective_vanish(pstyle, props.style_id.as_deref(), props)
    }

    /// The run's effective look, its styles resolved as the Rich Text
    /// writer resolves them.
    fn effective(&self, props: &RunProps, pstyle: Option<&str>) -> RunProps {
        effective_run(self.styles, props, pstyle)
    }

    fn run(&mut self, text: &str, props: &RunProps, pstyle: Option<&str>) {
        if text.is_empty() || self.hidden(props, pstyle) {
            return;
        }
        let css = self.run_css(props, pstyle);
        if css.is_empty() {
            self.text(text);
        } else {
            let _ = write!(self.out, "<span style=\"{}\">", attr(&css));
            self.text(text);
            self.out.push_str("</span>");
        }
    }

    /// A run's look as CSS.
    fn run_css(&self, props: &RunProps, pstyle: Option<&str>) -> String {
        let p = self.effective(props, pstyle);
        let mut css: Vec<String> = Vec::new();
        if p.bold {
            css.push("font-weight:bold".into());
        } else if self.in_heading {
            css.push("font-weight:normal".into());
        }
        if p.italic {
            css.push("font-style:italic".into());
        }
        match (p.underline, p.strike) {
            (true, true) => css.push("text-decoration:underline line-through".into()),
            (true, false) => css.push("text-decoration:underline".into()),
            (false, true) => css.push("text-decoration:line-through".into()),
            (false, false) => {}
        }
        // Inline code with no font of its own is monospace, over the
        // document's default font.
        let font = p.font.as_deref().map(font_name).filter(|f| !f.is_empty());
        match font {
            _ if p.code && props.font.is_none() => {
                css.push("font-family:'Courier New',monospace".into())
            }
            Some(f) => css.push(format!("font-family:'{f}'")),
            None => {}
        }
        if let Some(size) = p.size_half_pts.filter(|&s| s > 0) {
            css.push(format!("font-size:{}pt", num(f64::from(size) / 2.0, 1)));
        }
        if let Some(c) = p
            .color
            .as_deref()
            .filter(|c| c.len() == 6 && c.chars().all(|ch| ch.is_ascii_hexdigit()))
        {
            css.push(format!("color:#{}", c.to_ascii_uppercase()));
        }
        if let Some(name) = p.highlight.as_deref() {
            if let Some((r, g, b)) = highlight_rgb(name) {
                // `mso-highlight` for Outlook and the HTML import, the
                // background for every other client.
                css.push(format!(
                    "background-color:#{r:02X}{g:02X}{b:02X};mso-highlight:{name}"
                ));
            }
        }
        match p.vert_align {
            VertAlign::Superscript => css.push("vertical-align:super".into()),
            VertAlign::Subscript => css.push("vertical-align:sub".into()),
            VertAlign::Baseline => {}
        }
        if p.caps {
            css.push("text-transform:uppercase".into());
        }
        if p.small_caps {
            css.push("font-variant:small-caps".into());
        }
        css.join(";")
    }

    /// Text as HTML: escaped, a line feed as `<br>`, other control
    /// characters dropped, and a space that would collapse (at the line's
    /// start, after another space) as `&nbsp;`.
    fn text(&mut self, s: &str) {
        for ch in s.chars() {
            match ch {
                ' ' if self.text.after_space => {
                    self.out.push_str("&nbsp;");
                    self.text.trailing_space = None;
                    self.mark_shown();
                }
                ' ' => {
                    self.text.trailing_space = Some(self.out.len());
                    self.out.push(' ');
                    self.text.after_space = true;
                    self.mark_shown();
                }
                '\n' => self.line_break(),
                '\t' => {
                    self.out
                        .push_str("<span style=\"mso-tab-count:1;white-space:pre\">\t</span>");
                    self.shown_solid();
                }
                c if c.is_control() => {}
                c => {
                    match c {
                        '&' => self.out.push_str("&amp;"),
                        '<' => self.out.push_str("&lt;"),
                        '>' => self.out.push_str("&gt;"),
                        c => self.out.push(c),
                    }
                    self.shown_solid();
                }
            }
        }
    }

    /// Something visible was written: the paragraph is not empty, and
    /// [`Writer::shown`] counts it.
    fn mark_shown(&mut self) {
        self.text.shown = true;
        self.shown += 1;
    }

    /// Something that is not a space was shown.
    fn shown_solid(&mut self) {
        self.text.after_space = false;
        self.text.trailing_space = None;
        self.mark_shown();
    }

    fn line_break(&mut self) {
        if let Some(at) = self.text.trailing_space.take() {
            self.out.replace_range(at..at + 1, "&nbsp;");
        }
        self.out.push_str("<br>");
        self.text.after_space = true;
        self.mark_shown();
    }

    /// An inline picture: an `<img>` of its media part's `cid:`, at its
    /// size in pixels; its Alt Text when the package does not hold it.
    fn picture(&mut self, raw: &str, rid: &str) {
        let Some((cx, cy)) = crate::image::inline_extent(raw) else {
            return;
        };
        let px = |emu: i64| {
            let n = (emu as f64 / crate::image::EMU_PER_PX as f64).round();
            (n.max(1.0)).min(f64::from(u32::MAX)) as u32
        };
        let (w, h) = (px(cx), px(cy));
        let alt = alt_text(raw);
        let Some(i) = self.medium(rid) else {
            self.text(&alt);
            return;
        };
        let id = self.media[i].content_id.clone();
        let _ = write!(
            self.out,
            "<img src=\"cid:{}\" width=\"{w}\" height=\"{h}\" alt=\"{}\" \
             style=\"width:{w}px;height:{h}px;border:0\">",
            attr(&id),
            attr(&alt)
        );
        self.shown_solid();
    }

    /// The index in `media` of the part `rid` names, added on first use;
    /// `None` without a package, a relationship or the part.
    fn medium(&mut self, rid: &str) -> Option<usize> {
        let pkg = self.pkg?;
        let name = pkg.part_rel_target(DOCUMENT_PART, rid)?;
        if let Some(&i) = self.media_index.get(&name) {
            return Some(i);
        }
        let bytes = pkg.part(&name)?.to_vec();
        let file_name = name.rsplit('/').next().unwrap_or(&name).to_string();
        let ext = file_name.rsplit_once('.').map_or("", |(_, e)| e);
        let content_type = pkg
            .part_content_type(&name)
            .or_else(|| crate::image::media_content_type(ext).map(str::to_string))
            .unwrap_or_else(|| "application/octet-stream".to_string());
        // Distinct per picture and the same on every export.
        let content_id = format!("image{}.{:016x}@docxy", self.media.len() + 1, fnv1a(&bytes));
        self.media.push(EmailMedia {
            content_id,
            bytes,
            file_name,
            content_type,
        });
        self.media_index.insert(name, self.media.len() - 1);
        Some(self.media.len() - 1)
    }
}

impl ListKind {
    fn from_fmt(fmt: NumFmt) -> ListKind {
        match fmt {
            NumFmt::Bullet => ListKind::Bullet,
            NumFmt::Decimal => ListKind::Ordered("1"),
            NumFmt::LowerLetter => ListKind::Ordered("a"),
            NumFmt::UpperLetter => ListKind::Ordered("A"),
            NumFmt::LowerRoman => ListKind::Ordered("i"),
            NumFmt::UpperRoman => ListKind::Ordered("I"),
        }
    }
}

/// How many rows below `row` continue the vertical merge that starts at
/// grid column `col`.
fn continued_rows(t: &Table, map: &GridMap, row: usize, col: usize) -> usize {
    let mut n = 0;
    for (r, below) in t.rows.iter().enumerate().skip(row + 1) {
        let continues = map.rows[r]
            .cell_starting(col)
            .and_then(|ci| below.cells.get(ci))
            .is_some_and(|c| c.v_merge == VMerge::Continue);
        if !continues {
            break;
        }
        n += 1;
    }
    n
}

/// The `href` a link is written with: its target when its scheme is safe,
/// else its in-document anchor; `None` (its text only) otherwise.
fn link_href(h: &Hyperlink) -> Option<String> {
    if let Some(t) = h.target.as_deref().map(str::trim) {
        if let Some((scheme, _)) = t.split_once(':') {
            let scheme = scheme.to_ascii_lowercase();
            if crate::import::html::LINK_SCHEMES.contains(&scheme.as_str()) {
                return Some(t.to_string());
            }
        }
    }
    h.anchor
        .as_deref()
        .filter(|a| !a.is_empty())
        .map(|a| format!("#{a}"))
}

/// A picture's Alt Text: its `wp:docPr`'s `descr`, empty when none.
fn alt_text(raw: &str) -> String {
    crate::load::start_tags(raw, "wp:docPr")
        .first()
        .and_then(|(_, el)| crate::load::xml_attr_value(el, "descr"))
        .map(|d| crate::field::xml_unescape(&d))
        .unwrap_or_default()
}

/// A font name as a CSS string's content: no quote, backslash or `;` that
/// would end it, the string or the declaration.
fn font_name(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\' | ';') && !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// `s` as an attribute value inside `"…"`: `&`, `<`, `>` and `"` escaped,
/// control characters dropped.
fn attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Twips as points, up to two decimals (a twip is 0.05 pt, so exactly).
fn pt(twips: i32) -> String {
    num(f64::from(twips) / 20.0, 2)
}

/// `v` with at most `places` decimals, trailing zeros dropped.
fn num(v: f64, places: usize) -> String {
    let s = format!("{v:.places$}");
    if s.contains('.') {
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s == "-0" { "0".into() } else { s.into() }
    } else {
        s
    }
}

/// FNV-1a, 64 bits.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::ImportedMedia;
    use crate::import::html::import_html_with;
    use crate::model::{BreakKind, ParProps, Row, Run, Spacing};
    use crate::package::new_markdown_package;

    fn run(s: &str, props: RunProps) -> Inline {
        Inline::Run(Run {
            text: s.into(),
            props,
        })
    }

    fn plain(s: &str) -> Inline {
        run(s, RunProps::default())
    }

    fn para(props: ParProps, content: Vec<Inline>) -> Block {
        Block::Paragraph(Paragraph { props, content })
    }

    fn text_para(s: &str) -> Block {
        para(ParProps::default(), vec![plain(s)])
    }

    fn item(num_id: i32, ilvl: i32, s: &str) -> Block {
        para(
            ParProps {
                num_id: Some(num_id),
                ilvl,
                ..Default::default()
            },
            vec![plain(s)],
        )
    }

    fn doc(body: Vec<Block>) -> Document {
        Document { body }
    }

    fn html(d: &Document) -> String {
        to_email_html(d, None).html
    }

    /// Export, then import, each `cid:` picture read from the export's
    /// media.
    fn back_with(d: &Document, pkg: Option<&Package>) -> (Document, Vec<ImportedMedia>) {
        let e = to_email_html(d, pkg);
        let media = e.media.clone();
        let mut resolve = |src: &str, _: usize| {
            media
                .iter()
                .find(|m| src == format!("cid:{}", m.content_id))
                .map(|m| m.bytes.clone())
        };
        let imported = import_html_with(e.html.as_bytes(), &mut resolve).expect("imports");
        (imported.document, imported.media)
    }

    fn back(d: &Document) -> Document {
        back_with(d, None).0
    }

    /// Every paragraph, table cells' included, in order.
    fn paras(d: &Document) -> Vec<Paragraph> {
        fn walk(blocks: &[Block], out: &mut Vec<Paragraph>) {
            for b in blocks {
                match b {
                    Block::Paragraph(p) => out.push(p.clone()),
                    Block::Table(t) => {
                        for row in &t.rows {
                            for cell in &row.cells {
                                walk(&cell.blocks, out);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&d.body, &mut out);
        out
    }

    /// The paragraph whose text contains `needle`.
    fn para_of(d: &Document, needle: &str) -> Paragraph {
        paras(d)
            .into_iter()
            .find(|p| p.plain_text().contains(needle))
            .unwrap_or_else(|| panic!("no paragraph {needle:?} in {d:?}"))
    }

    /// The run (a link's included) whose text contains `needle`.
    fn run_of(d: &Document, needle: &str) -> RunProps {
        for p in paras(d) {
            for i in &p.content {
                let runs: Vec<&Run> = match i {
                    Inline::Run(r) => vec![r],
                    Inline::Hyperlink(h) => h.runs.iter().collect(),
                    _ => vec![],
                };
                if let Some(r) = runs.into_iter().find(|r| r.text.contains(needle)) {
                    return r.props.clone();
                }
            }
        }
        panic!("no run {needle:?} in {d:?}")
    }

    /// One run `"x"` of `props` beside a plain `"n"`, exported and read back.
    fn run_back(props: RunProps) -> (RunProps, RunProps) {
        let d = back(&doc(vec![para(
            ParProps::default(),
            vec![run("x", props), plain(" n")],
        )]));
        (run_of(&d, "x"), run_of(&d, "n"))
    }

    fn par_back(props: ParProps) -> ParProps {
        para_of(&back(&doc(vec![para(props, vec![plain("body")])])), "body").props
    }

    // ---- runs ----

    #[test]
    fn bold_round_trips() {
        let (x, n) = run_back(RunProps {
            bold: true,
            ..Default::default()
        });
        assert!(x.bold && !n.bold);
    }

    #[test]
    fn italic_round_trips() {
        let (x, n) = run_back(RunProps {
            italic: true,
            ..Default::default()
        });
        assert!(x.italic && !n.italic);
    }

    #[test]
    fn underline_round_trips() {
        let (x, n) = run_back(RunProps {
            underline: true,
            ..Default::default()
        });
        assert!(x.underline && !x.strike && !n.underline);
    }

    #[test]
    fn strike_round_trips() {
        let (x, n) = run_back(RunProps {
            strike: true,
            ..Default::default()
        });
        assert!(x.strike && !x.underline && !n.strike);
        let (both, _) = run_back(RunProps {
            underline: true,
            strike: true,
            ..Default::default()
        });
        assert!(both.underline && both.strike);
    }

    #[test]
    fn font_family_round_trips() {
        let (x, _) = run_back(RunProps {
            font: Some("Georgia".into()),
            ..Default::default()
        });
        assert_eq!(x.font.as_deref(), Some("Georgia"));
        let (spaced, _) = run_back(RunProps {
            font: Some("Times New Roman".into()),
            ..Default::default()
        });
        assert_eq!(spaced.font.as_deref(), Some("Times New Roman"));
    }

    #[test]
    fn font_size_round_trips() {
        for half in [21, 24, 56] {
            let (x, _) = run_back(RunProps {
                size_half_pts: Some(half),
                ..Default::default()
            });
            assert_eq!(x.size_half_pts, Some(half));
        }
    }

    #[test]
    fn colour_round_trips() {
        let (x, n) = run_back(RunProps {
            color: Some("C00000".into()),
            ..Default::default()
        });
        assert_eq!(x.color.as_deref(), Some("C00000"));
        assert_eq!(n.color, None);
    }

    /// `background-color` also imports as run shading (`w:shd`), so only
    /// the highlight is asserted; it is written for the clients that ignore
    /// `mso-highlight`.
    #[test]
    fn highlight_round_trips() {
        for name in ["yellow", "darkBlue"] {
            let (x, n) = run_back(RunProps {
                highlight: Some(name.into()),
                ..Default::default()
            });
            assert_eq!(x.highlight.as_deref(), Some(name));
            assert_eq!(n.highlight, None);
        }
        let h = html(&doc(vec![para(
            ParProps::default(),
            vec![run(
                "x",
                RunProps {
                    highlight: Some("yellow".into()),
                    ..Default::default()
                },
            )],
        )]));
        assert!(
            h.contains("background-color:#FFFF00;mso-highlight:yellow"),
            "{h}"
        );
    }

    #[test]
    fn caps_small_caps_and_code_are_written() {
        let h = html(&doc(vec![para(
            ParProps::default(),
            vec![
                run(
                    "caps",
                    RunProps {
                        caps: true,
                        ..Default::default()
                    },
                ),
                run(
                    "small",
                    RunProps {
                        small_caps: true,
                        ..Default::default()
                    },
                ),
                run(
                    "code",
                    RunProps {
                        code: true,
                        ..Default::default()
                    },
                ),
            ],
        )]));
        assert!(h.contains("text-transform:uppercase\">caps<"), "{h}");
        assert!(h.contains("font-variant:small-caps\">small<"), "{h}");
        assert!(h.contains("font-family:'Courier New',monospace"), "{h}");
    }

    #[test]
    fn superscript_and_subscript_round_trip() {
        let (x, _) = run_back(RunProps {
            vert_align: VertAlign::Superscript,
            ..Default::default()
        });
        assert_eq!(x.vert_align, VertAlign::Superscript);
        let (x, _) = run_back(RunProps {
            vert_align: VertAlign::Subscript,
            ..Default::default()
        });
        assert_eq!(x.vert_align, VertAlign::Subscript);
    }

    #[test]
    fn hidden_text_is_not_written() {
        let h = html(&doc(vec![para(
            ParProps::default(),
            vec![
                plain("shown "),
                run(
                    "secret",
                    RunProps {
                        vanish: true,
                        ..Default::default()
                    },
                ),
                Inline::Tab(RunProps {
                    vanish: true,
                    ..Default::default()
                }),
            ],
        )]));
        assert!(h.contains("shown") && !h.contains("secret"), "{h}");
        assert!(!h.contains("mso-tab-count"), "{h}");
    }

    /// Text a character or paragraph style hides never leaves in a message.
    #[test]
    fn style_hidden_text_is_not_written() {
        let mut pkg = new_markdown_package(Document::default());
        let styles = pkg.part_text("word/styles.xml").expect("styles");
        let styles = styles.replace(
            "</w:styles>",
            "<w:style w:type=\"character\" w:styleId=\"Secret\"><w:name w:val=\"Secret\"/>\
             <w:rPr><w:vanish/></w:rPr></w:style>\
             <w:style w:type=\"paragraph\" w:styleId=\"HiddenPara\"><w:name w:val=\"HiddenPara\"/>\
             <w:rPr><w:vanish/></w:rPr></w:style></w:styles>",
        );
        assert!(pkg.set_part_text("word/styles.xml", &styles));
        let d = doc(vec![
            para(
                ParProps::default(),
                vec![
                    plain("shown "),
                    run(
                        "charsecret",
                        RunProps {
                            style_id: Some("Secret".into()),
                            ..Default::default()
                        },
                    ),
                ],
            ),
            para(
                ParProps {
                    style_id: Some("HiddenPara".into()),
                    ..Default::default()
                },
                vec![plain("parasecret")],
            ),
        ]);
        let h = to_email_html(&d, Some(&pkg)).html;
        assert!(h.contains("shown"), "{h}");
        assert!(
            !h.contains("charsecret") && !h.contains("parasecret"),
            "{h}"
        );
    }

    // ---- paragraphs ----

    #[test]
    fn alignment_round_trips() {
        for align in [Align::Left, Align::Center, Align::Right, Align::Justify] {
            let p = par_back(ParProps {
                align,
                ..Default::default()
            });
            assert_eq!(p.align, align);
        }
    }

    #[test]
    fn left_and_right_indent_round_trip() {
        let p = par_back(ParProps {
            indent: 720,
            indent_right: 360,
            ..Default::default()
        });
        assert_eq!((p.indent, p.indent_right), (720, 360));
    }

    #[test]
    fn first_line_and_hanging_indent_round_trip() {
        let p = par_back(ParProps {
            first_line: 360,
            ..Default::default()
        });
        assert_eq!((p.indent, p.first_line), (0, 360));
        let p = par_back(ParProps {
            indent: 720,
            first_line: -360,
            ..Default::default()
        });
        assert_eq!((p.indent, p.first_line), (720, -360));
    }

    /// An unset spacing is written `margin:0` (so a client's own `p`
    /// margin never shows) and imports as `Some(0)`: only set values are
    /// asserted.
    #[test]
    fn spacing_before_and_after_round_trip() {
        let p = par_back(ParProps {
            spacing: Spacing {
                before: Some(240),
                after: Some(130),
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!((p.spacing.before, p.spacing.after), (Some(240), Some(130)));
    }

    #[test]
    fn line_spacing_round_trips() {
        for (line, rule) in [
            (360, "auto"),
            (259, "auto"),
            (300, "exact"),
            (280, "atLeast"),
        ] {
            let p = par_back(ParProps {
                spacing: Spacing {
                    line: Some(line),
                    line_rule: Some(rule.into()),
                    ..Default::default()
                },
                ..Default::default()
            });
            assert_eq!(
                (p.spacing.line, p.spacing.line_rule.as_deref()),
                (Some(line), Some(rule)),
                "{line} {rule}"
            );
        }
    }

    #[test]
    fn tab_round_trips() {
        let d = back(&doc(vec![para(
            ParProps::default(),
            vec![plain("a"), Inline::Tab(RunProps::default()), plain("b")],
        )]));
        let p = para_of(&d, "a");
        let kinds: Vec<&str> = p
            .content
            .iter()
            .map(|i| match i {
                Inline::Run(_) => "run",
                Inline::Tab(_) => "tab",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["run", "tab", "run"]);
    }

    #[test]
    fn a_right_to_left_paragraph_has_its_direction() {
        let h = html(&doc(vec![para(
            ParProps {
                rtl: true,
                ..Default::default()
            },
            vec![plain("rtl")],
        )]));
        assert!(h.contains("<p dir=\"rtl\""), "{h}");
        // Its start is its right edge, as the renderer aligns it.
        assert!(h.contains("text-align:right"), "{h}");
    }

    #[test]
    fn headings_are_heading_elements() {
        let d = doc(vec![para(
            ParProps {
                heading_level: Some(2),
                style_id: Some("Heading2".into()),
                ..Default::default()
            },
            vec![plain("Title")],
        )]);
        let h = html(&d);
        assert!(h.contains("<h2 style="), "{h}");
        assert_eq!(para_of(&back(&d), "Title").props.heading_level, Some(2));
    }

    #[test]
    fn an_empty_paragraph_keeps_its_line() {
        let h = html(&doc(vec![para(ParProps::default(), vec![])]));
        assert!(h.contains(">&nbsp;</p>"), "{h}");
    }

    // ---- lists ----

    #[test]
    fn bullet_list_round_trips() {
        let d = back(&doc(vec![item(1, 0, "one"), item(1, 0, "two")]));
        for t in ["one", "two"] {
            let p = para_of(&d, t);
            assert_eq!((p.props.num_id, p.props.ilvl), (Some(1), 0), "{t}");
        }
        let h = html(&doc(vec![item(1, 0, "one")]));
        assert!(h.contains("<ul "), "{h}");
    }

    #[test]
    fn numbered_list_round_trips() {
        let d = back(&doc(vec![item(2, 0, "one"), item(2, 0, "two")]));
        for t in ["one", "two"] {
            assert_eq!(para_of(&d, t).props.num_id, Some(2), "{t}");
        }
        let h = html(&doc(vec![item(2, 0, "one")]));
        assert!(h.contains("<ol type=\"1\""), "{h}");
    }

    #[test]
    fn nested_levels_round_trip() {
        let d = back(&doc(vec![
            item(1, 0, "zero"),
            item(1, 1, "one"),
            item(2, 2, "two"),
            item(1, 0, "back"),
            item(2, 2, "jump"),
        ]));
        let got: Vec<(Option<i32>, i32)> = ["zero", "one", "two", "back", "jump"]
            .iter()
            .map(|t| {
                let p = para_of(&d, t);
                (p.props.num_id, p.props.ilvl)
            })
            .collect();
        assert_eq!(
            got,
            [
                (Some(1), 0),
                (Some(1), 1),
                (Some(2), 2),
                (Some(1), 0),
                (Some(2), 2)
            ]
        );
    }

    #[test]
    fn a_numbered_heading_is_a_heading() {
        let d = doc(vec![para(
            ParProps {
                heading_level: Some(1),
                num_id: Some(2),
                ..Default::default()
            },
            vec![plain("Chapter")],
        )]);
        let h = html(&d);
        assert!(h.contains("<h1 ") && !h.contains("<ol"), "{h}");
    }

    #[test]
    fn two_lists_in_a_row_stay_two_lists() {
        let d = back(&doc(vec![item(1, 0, "dot"), item(2, 0, "num")]));
        assert_eq!(para_of(&d, "dot").props.num_id, Some(1));
        assert_eq!(para_of(&d, "num").props.num_id, Some(2));
    }

    /// The `<ol …>` start tags of `h`, in order.
    fn ol_tags(h: &str) -> Vec<String> {
        h.match_indices("<ol ")
            .map(|(at, _)| h[at..at + h[at..].find('>').unwrap() + 1].to_string())
            .collect()
    }

    #[test]
    fn a_different_num_id_reopens_the_list() {
        let mut pkg = new_markdown_package(Document::default());
        let numbering = pkg.part_text("word/numbering.xml").expect("numbering");
        let numbering = numbering.replace(
            "</w:numbering>",
            "<w:num w:numId=\"3\"><w:abstractNumId w:val=\"101\"/></w:num></w:numbering>",
        );
        assert!(pkg.set_part_text("word/numbering.xml", &numbering));
        let d = doc(vec![item(2, 0, "a"), item(3, 0, "b")]);
        let h = to_email_html(&d, Some(&pkg)).html;
        assert_eq!(ol_tags(&h).len(), 2, "{h}");
        assert_eq!(h.matches("</ol>").count(), 2, "{h}");
    }

    #[test]
    fn numbered_list_continues_after_a_paragraph() {
        let h = html(&doc(vec![
            item(2, 0, "a"),
            item(2, 0, "b"),
            text_para("between"),
            item(2, 0, "c"),
        ]));
        let tags = ol_tags(&h);
        assert_eq!(tags.len(), 2, "{h}");
        assert!(!tags[0].contains("start="), "{h}");
        assert!(tags[1].contains("start=\"3\""), "{h}");
    }

    #[test]
    fn a_shallower_item_restarts_deeper_numbering() {
        let h = html(&doc(vec![
            item(2, 0, "a"),
            item(2, 1, "a1"),
            text_para("between"),
            item(2, 1, "a2"),
            item(2, 0, "b"),
            text_para("again"),
            item(2, 1, "b1"),
        ]));
        let starts: Vec<Option<String>> = ol_tags(&h)
            .iter()
            .map(|t| {
                t.split("start=\"")
                    .nth(1)
                    .map(|s| s[..s.find('"').unwrap()].to_string())
            })
            .collect();
        // a, a1 | (level 0 between), a2 at 2 | (level 0 between), b1 at 1.
        let s = |v: &str| Some(v.to_string());
        assert_eq!(starts, [None, None, None, s("2"), None, None], "{h}");
        // b reuses the level-0 list opened between levels for a2, which
        // shows 1 next: b says its own number, 2, as Word shows it.
        assert!(h.contains("<li value=\"2\" style="), "{h}");
        assert!(
            h.split("value=\"2\"").nth(1).unwrap().contains(">b<"),
            "{h}"
        );
        assert_eq!(h.matches(" value=").count(), 1, "{h}");
    }

    /// A list opened between levels shows 1 next: an item numbered 1 there
    /// needs no `value`.
    #[test]
    fn an_item_in_a_list_opened_between_levels_numbered_one_has_no_value() {
        let h = html(&doc(vec![item(2, 1, "deep"), item(2, 0, "top")]));
        assert_eq!(ol_tags(&h).len(), 2, "{h}");
        assert!(!h.contains(" value=") && !h.contains("start="), "{h}");
    }

    /// Markers are looked up on the final view: accepting a deleted
    /// paragraph mark shifts every path after it.
    #[test]
    fn tracked_changes_are_their_final_view() {
        let mut first = ParProps::default();
        first
            .mark_revisions
            .push(crate::model::ParagraphMarkRevision {
                kind: RevisionKind::Delete,
                metadata: Default::default(),
            });
        let mut pkg = new_markdown_package(Document::default());
        let numbering = pkg.part_text("word/numbering.xml").expect("numbering");
        // numId 5 defines level 0 only: level 1's kind is its marker's
        // (`1.`, from the default `%1.`).
        let numbering = numbering.replace(
            "</w:numbering>",
            "<w:abstractNum w:abstractNumId=\"105\"><w:lvl w:ilvl=\"0\">\
             <w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%1.\"/></w:lvl></w:abstractNum>\
             <w:num w:numId=\"5\"><w:abstractNumId w:val=\"105\"/></w:num></w:numbering>",
        );
        assert!(pkg.set_part_text("word/numbering.xml", &numbering));
        let d = doc(vec![
            para(first, vec![plain("Hello")]),
            text_para(" world"),
            item(5, 1, "numbered"),
        ]);
        let h = to_email_html(&d, Some(&pkg)).html;
        // One paragraph, the deleted mark joining them.
        let (back, _) = back_with(&d, Some(&pkg));
        assert_eq!(para_of(&back, "Hello").plain_text(), "Hello world");
        assert!(h.contains("<ol type=\"1\""), "{h}");
        assert!(!h.contains("<ul"), "{h}");
    }

    // ---- links ----

    fn link(target: Option<&str>, anchor: Option<&str>, text: &str) -> Inline {
        Inline::Hyperlink(Hyperlink {
            target: target.map(str::to_string),
            anchor: anchor.map(str::to_string),
            runs: vec![Run {
                text: text.into(),
                props: RunProps::default(),
            }],
            ..Default::default()
        })
    }

    fn links(d: &Document) -> Vec<(Option<String>, Option<String>, String)> {
        paras(d)
            .iter()
            .flat_map(|p| p.content.clone())
            .filter_map(|i| match i {
                Inline::Hyperlink(h) => Some((
                    h.target.clone(),
                    h.anchor.clone(),
                    h.runs.iter().map(|r| r.text.as_str()).collect(),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn hyperlink_round_trips() {
        let d = back(&doc(vec![para(
            ParProps::default(),
            vec![
                plain("see "),
                link(Some("https://example.com/a?b=1"), None, "the site"),
                plain(" or "),
                link(Some("mailto:someone@example.com"), None, "write"),
                plain(" or "),
                link(None, Some("part2"), "below"),
            ],
        )]));
        let s = |v: &str| Some(v.to_string());
        assert_eq!(
            links(&d),
            [
                (s("https://example.com/a?b=1"), None, "the site".to_string()),
                (s("mailto:someone@example.com"), None, "write".to_string()),
                (None, s("part2"), "below".to_string()),
            ]
        );
    }

    #[test]
    fn unsafe_link_targets_are_text() {
        let targets = [
            "javascript:alert(1)",
            " JavaScript:alert(1)",
            "data:text/html,x",
            "file:///etc/passwd",
            "vbscript:x",
            "relative/page.html",
        ];
        let content: Vec<Inline> = targets
            .iter()
            .enumerate()
            .map(|(i, t)| link(Some(t), None, &format!("text{i} ")))
            .collect();
        let h = html(&doc(vec![para(ParProps::default(), content)]));
        assert!(!h.contains("<a "), "{h}");
        for i in 0..targets.len() {
            assert!(h.contains(&format!("text{i}")), "{h}");
        }
    }

    fn link_of(runs: Vec<Run>, content: Vec<Inline>) -> Inline {
        Inline::Hyperlink(Hyperlink {
            target: Some("https://secret.test/x".into()),
            runs,
            content,
            ..Default::default()
        })
    }

    fn hidden_run(text: &str) -> Run {
        Run {
            text: text.into(),
            props: RunProps {
                vanish: true,
                ..Default::default()
            },
        }
    }

    /// Its text hidden, a link writes nothing: its URL never leaves.
    #[test]
    fn a_hidden_link_writes_no_url() {
        let h = html(&doc(vec![para(
            ParProps::default(),
            vec![
                plain("before "),
                link_of(vec![hidden_run("x")], vec![]),
                plain("after"),
            ],
        )]));
        assert!(!h.contains("secret.test") && !h.contains("<a "), "{h}");
        // The text around it is as it was: the space before stays a space.
        assert!(h.contains(">before </span><span"), "{h}");
        // An anchor-only link too.
        let anchor = Inline::Hyperlink(Hyperlink {
            anchor: Some("hidden_place".into()),
            runs: vec![hidden_run("y")],
            ..Default::default()
        });
        let h = html(&doc(vec![para(ParProps::default(), vec![anchor])]));
        assert!(!h.contains("hidden_place"), "{h}");
        assert!(h.contains(">&nbsp;</p>"), "the paragraph is empty: {h}");
    }

    #[test]
    fn a_style_hidden_link_writes_no_url() {
        let mut pkg = new_markdown_package(Document::default());
        let styles = pkg.part_text("word/styles.xml").expect("styles");
        let styles = styles.replace(
            "</w:styles>",
            "<w:style w:type=\"character\" w:styleId=\"Secret\"><w:name w:val=\"Secret\"/>\
             <w:rPr><w:vanish/></w:rPr></w:style></w:styles>",
        );
        assert!(pkg.set_part_text("word/styles.xml", &styles));
        let styled = Run {
            text: "x".into(),
            props: RunProps {
                style_id: Some("Secret".into()),
                ..Default::default()
            },
        };
        let d = doc(vec![para(
            ParProps::default(),
            vec![plain("text"), link_of(vec![styled], vec![])],
        )]);
        let h = to_email_html(&d, Some(&pkg)).html;
        assert!(!h.contains("secret.test"), "{h}");
    }

    #[test]
    fn a_deleted_link_writes_no_url() {
        let deleted = Inline::Revision {
            kind: RevisionKind::Delete,
            metadata: Default::default(),
            raw: String::new(),
            content: vec![plain("gone")],
            content_changed: false,
        };
        let h = html(&doc(vec![para(
            ParProps::default(),
            vec![plain("text"), link_of(vec![], vec![deleted])],
        )]));
        assert!(!h.contains("secret.test") && !h.contains("gone"), "{h}");
        // Accepting the deletion above drops the link whole; a tracked move
        // is left in place and writes nothing, so its link shows nothing.
        let moved = Inline::UnsupportedRevision {
            kind: crate::model::UnsupportedRevisionKind::MoveFrom,
            metadata: Default::default(),
            raw: "<w:moveFrom><w:r><w:t>moved</w:t></w:r></w:moveFrom>".into(),
        };
        let h = html(&doc(vec![para(
            ParProps::default(),
            vec![plain("text"), link_of(vec![], vec![moved])],
        )]));
        assert!(!h.contains("secret.test") && !h.contains("moved"), "{h}");
    }

    #[test]
    fn a_link_with_some_hidden_text_shows_the_rest() {
        let h = html(&doc(vec![para(
            ParProps::default(),
            vec![link_of(
                vec![
                    hidden_run("secret"),
                    Run {
                        text: "shown".into(),
                        props: RunProps::default(),
                    },
                ],
                vec![],
            )],
        )]));
        assert!(h.contains("<a href=\"https://secret.test/x\">"), "{h}");
        assert!(
            h.contains("shown</span></a>") && !h.contains(">secret<"),
            "{h}"
        );
    }

    // ---- pictures ----

    /// A PNG header of `w` x `h` pixels (all a sniff reads).
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        b
    }

    fn picture(rid: &str, w_px: i64, h_px: i64, alt: &str) -> Inline {
        Inline::Raw(crate::image::inline_picture_run_described(
            rid,
            w_px * crate::image::EMU_PER_PX,
            h_px * crate::image::EMU_PER_PX,
            1,
            "image1.png",
            alt,
            None,
        ))
    }

    /// A package holding `bytes` as a PNG part, and its document made by
    /// `body` from the part's rid.
    fn with_picture(bytes: &[u8], body: impl Fn(&str) -> Vec<Block>) -> Package {
        let mut pkg = new_markdown_package(Document::default());
        let rid = pkg
            .add_part_media(DOCUMENT_PART, bytes, "png", "image/png")
            .expect("added");
        pkg.document = doc(body(&rid));
        pkg
    }

    fn cids(h: &str) -> Vec<String> {
        h.match_indices("src=\"cid:")
            .map(|(at, m)| {
                let rest = &h[at + m.len()..];
                rest[..rest.find('"').unwrap()].to_string()
            })
            .collect()
    }

    #[test]
    fn picture_is_one_cid_img_and_one_media_entry() {
        let bytes = png(40, 30);
        let pkg = with_picture(&bytes, |rid| {
            vec![para(
                ParProps::default(),
                vec![plain("look "), picture(rid, 40, 30, "")],
            )]
        });
        let e = to_email_html(&pkg.document, Some(&pkg));
        assert_eq!(e.html.matches("<img ").count(), 1, "{}", e.html);
        let ids = cids(&e.html);
        assert_eq!(e.media.len(), 1);
        let m = &e.media[0];
        assert_eq!(ids, std::slice::from_ref(&m.content_id));
        assert!(m.content_id.starts_with("image1.") && m.content_id.ends_with("@docxy"));
        assert_eq!(m.bytes, bytes);
        assert_eq!(m.file_name, "image1.png");
        assert_eq!(m.content_type, "image/png");
        assert!(e.html.contains("width=\"40\" height=\"30\""), "{}", e.html);
    }

    #[test]
    fn picture_round_trips_size_and_bytes() {
        let bytes = png(120, 80);
        let pkg = with_picture(&bytes, |rid| {
            vec![para(ParProps::default(), vec![picture(rid, 200, 100, "")])]
        });
        let (d, media) = back_with(&pkg.document, Some(&pkg));
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].bytes, bytes);
        let raw = paras(&d)
            .into_iter()
            .flat_map(|p| p.content)
            .find_map(|i| match i {
                Inline::Raw(r) if crate::image::inline_embed(&r).is_some() => Some(r),
                _ => None,
            })
            .expect("a picture");
        assert_eq!(
            crate::image::inline_extent(&raw),
            Some((
                200 * crate::image::EMU_PER_PX,
                100 * crate::image::EMU_PER_PX
            ))
        );
    }

    #[test]
    fn a_picture_used_twice_is_one_medium() {
        let pkg = with_picture(&png(10, 10), |rid| {
            vec![
                para(ParProps::default(), vec![picture(rid, 10, 10, "")]),
                para(ParProps::default(), vec![picture(rid, 20, 20, "")]),
            ]
        });
        let e = to_email_html(&pkg.document, Some(&pkg));
        assert_eq!(e.html.matches("<img ").count(), 2, "{}", e.html);
        assert_eq!(e.media.len(), 1);
        let ids = cids(&e.html);
        assert_eq!(
            ids,
            [e.media[0].content_id.clone(), e.media[0].content_id.clone()]
        );
    }

    #[test]
    fn two_pictures_have_two_ids() {
        let mut pkg = new_markdown_package(Document::default());
        let a = pkg
            .add_part_media(DOCUMENT_PART, &png(1, 1), "png", "image/png")
            .unwrap();
        let b = pkg
            .add_part_media(DOCUMENT_PART, &png(2, 2), "png", "image/png")
            .unwrap();
        pkg.document = doc(vec![para(
            ParProps::default(),
            vec![picture(&a, 1, 1, ""), picture(&b, 2, 2, "")],
        )]);
        let e = to_email_html(&pkg.document, Some(&pkg));
        assert_eq!(e.media.len(), 2);
        assert_ne!(e.media[0].content_id, e.media[1].content_id);
        assert_eq!(
            cids(&e.html),
            [e.media[0].content_id.clone(), e.media[1].content_id.clone()]
        );
    }

    #[test]
    fn a_picture_without_media_is_its_alt_text() {
        let d = doc(vec![para(
            ParProps::default(),
            vec![picture("rId99", 10, 10, "A <cat>")],
        )]);
        let e = to_email_html(&d, None);
        assert!(e.media.is_empty());
        assert!(!e.html.contains("<img"), "{}", e.html);
        assert!(e.html.contains("A &lt;cat&gt;"), "{}", e.html);
    }

    #[test]
    fn a_picture_carries_its_alt_text() {
        let pkg = with_picture(&png(5, 5), |rid| {
            vec![para(
                ParProps::default(),
                vec![picture(rid, 5, 5, "Say \"hi\" & go")],
            )]
        });
        let h = to_email_html(&pkg.document, Some(&pkg)).html;
        assert!(h.contains("alt=\"Say &quot;hi&quot; &amp; go\""), "{h}");
    }

    // ---- the page ----

    fn table(rows: Vec<Vec<Cell>>) -> Block {
        Block::Table(Table {
            grid: vec![2000, 2000],
            rows: rows
                .into_iter()
                .map(|cells| Row {
                    cells,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
    }

    fn cell(text: &str, grid_span: u32, v_merge: VMerge) -> Cell {
        Cell {
            grid_span,
            v_merge,
            blocks: vec![text_para(text)],
            ..Default::default()
        }
    }

    #[test]
    fn table_cells_keep_their_content() {
        let d = doc(vec![table(vec![
            vec![cell("wide", 2, VMerge::None)],
            vec![
                cell("tall", 1, VMerge::Restart),
                cell("r2", 1, VMerge::None),
            ],
            vec![cell("", 1, VMerge::Continue), cell("r3", 1, VMerge::None)],
        ])]);
        let h = html(&d);
        assert!(h.contains("colspan=\"2\""), "{h}");
        assert!(h.contains("rowspan=\"2\""), "{h}");
        assert_eq!(h.matches("<td").count(), 4, "{h}");
        let back = back(&d);
        for t in ["wide", "tall", "r2", "r3"] {
            para_of(&back, t);
        }
    }

    #[test]
    fn the_page_is_a_whole_utf8_document() {
        let h = html(&doc(vec![text_para("Caf\u{e9}")]));
        assert!(h.starts_with("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">"));
        assert!(h.trim_end().ends_with("</body>\n</html>"), "{h}");
        assert!(h.contains("Caf\u{e9}"));
    }

    /// Every feature at once: no `<style>` element and no `class`
    /// attribute, which mail clients strip.
    #[test]
    fn no_style_element_or_class_attribute() {
        let pkg = with_picture(&png(8, 8), |rid| {
            vec![
                para(
                    ParProps {
                        heading_level: Some(1),
                        style_id: Some("Heading1".into()),
                        ..Default::default()
                    },
                    vec![plain("Title")],
                ),
                para(
                    ParProps {
                        align: Align::Center,
                        indent: 720,
                        first_line: -360,
                        spacing: Spacing {
                            before: Some(120),
                            line: Some(300),
                            line_rule: Some("exact".into()),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    vec![
                        run(
                            "styled",
                            RunProps {
                                bold: true,
                                italic: true,
                                underline: true,
                                strike: true,
                                font: Some("Georgia".into()),
                                size_half_pts: Some(28),
                                color: Some("112233".into()),
                                highlight: Some("green".into()),
                                ..Default::default()
                            },
                        ),
                        Inline::Tab(RunProps::default()),
                        link(Some("https://example.com"), None, "link"),
                        picture(rid, 8, 8, "dot"),
                    ],
                ),
                item(1, 0, "bullet"),
                item(2, 1, "number"),
                table(vec![vec![
                    cell("c1", 1, VMerge::None),
                    cell("c2", 1, VMerge::None),
                ]]),
            ]
        });
        let e = to_email_html(&pkg.document, Some(&pkg));
        let lower = e.html.to_ascii_lowercase();
        assert!(!lower.contains("<style"), "{}", e.html);
        assert!(!lower.contains("class="), "{}", e.html);
        for needle in [
            "<h1",
            "<ul",
            "<ol",
            "<a href",
            "<img",
            "<table",
            "mso-tab-count",
        ] {
            assert!(lower.contains(needle), "{needle}: {}", e.html);
        }
        assert_eq!(e.media.len(), 1);
    }

    #[test]
    fn text_and_attributes_are_escaped() {
        let target = "mailto:a@example.com?subject=x&body=y";
        let quoted = "https://example.com/?q=\"y\"";
        let d = doc(vec![para(
            ParProps::default(),
            vec![
                plain("a<b> & \"c\""),
                plain(" "),
                link(Some(target), None, "mail"),
                link(Some(quoted), None, "quoted"),
                run(
                    "font",
                    RunProps {
                        font: Some("Bad'\";Font".into()),
                        ..Default::default()
                    },
                ),
            ],
        )]);
        let h = html(&d);
        assert!(h.contains("a&lt;b&gt; &amp; \"c\""), "{h}");
        assert!(
            h.contains("href=\"mailto:a@example.com?subject=x&amp;body=y\""),
            "{h}"
        );
        assert!(
            h.contains("href=\"https://example.com/?q=&quot;y&quot;\""),
            "{h}"
        );
        assert!(h.contains("font-family:'BadFont'"), "{h}");
        let back = back(&d);
        assert!(para_of(&back, "a<b>").plain_text().contains("a<b> & \"c\""));
        assert_eq!(links(&back)[0].0.as_deref(), Some(target));
    }

    #[test]
    fn runs_of_spaces_survive() {
        let d = doc(vec![para(
            ParProps::default(),
            vec![
                plain("  lead"),
                plain(" two  spaces "),
                plain("and   three "),
            ],
        )]);
        let h = html(&d);
        assert!(!h.contains("  "), "no collapsible run of spaces: {h}");
        let text = para_of(&back(&d), "lead")
            .plain_text()
            .replace('\u{a0}', " ");
        assert_eq!(text, "  lead two  spaces and   three ");
    }

    #[test]
    fn breaks_footnotes_and_fields_are_written() {
        let h = html(&doc(vec![para(
            ParProps::default(),
            vec![
                plain("line"),
                Inline::Break(BreakKind::Line, RunProps::default()),
                plain("next"),
                Inline::FootnoteRef {
                    id: 3,
                    endnote: false,
                    raw: String::new(),
                },
                Inline::Field {
                    raw: "<w:fldSimple w:instr=\"PAGE\"><w:r><w:t>Page 3</w:t></w:r></w:fldSimple>"
                        .into(),
                    text: "Page 3".into(),
                },
                Inline::Equation {
                    raw: "<m:oMath><m:r><m:t>x</m:t></m:r></m:oMath>".into(),
                    text: "x\u{b2}".into(),
                    latex: None,
                },
                Inline::SmartArt {
                    raw: String::new(),
                    text: vec!["node".into()],
                },
            ],
        )]));
        assert!(h.contains("line</span><br><span"), "{h}");
        assert!(h.contains("next</span><sup>3</sup>"), "{h}");
        for text in ["Page 3", "x\u{b2}", "node"] {
            assert!(h.contains(&format!(">{text}</span>")), "{text}: {h}");
        }
    }

    /// `raw`, a `<w:r>`, with `rpr` as its run properties.
    fn with_rpr(raw: &str, rpr: &str) -> String {
        let raw = raw.strip_prefix("<w:r>").expect("a run");
        format!("<w:r><w:rPr>{rpr}</w:rPr>{raw}")
    }

    fn hidden_picture(rid: &str, rpr: &str) -> Inline {
        let Inline::Raw(raw) = picture(rid, 4, 4, "alt words") else {
            unreachable!()
        };
        Inline::Raw(with_rpr(&raw, rpr))
    }

    #[test]
    fn a_hidden_picture_is_not_written_or_attached() {
        let pkg = with_picture(&png(4, 4), |rid| {
            vec![para(
                ParProps::default(),
                vec![plain("text "), hidden_picture(rid, "<w:vanish/>")],
            )]
        });
        let e = to_email_html(&pkg.document, Some(&pkg));
        assert!(
            !e.html.contains("<img") && !e.html.contains("alt words"),
            "{}",
            e.html
        );
        assert!(e.media.is_empty());
        // Switched off, it shows.
        let pkg = with_picture(&png(4, 4), |rid| {
            vec![para(
                ParProps::default(),
                vec![hidden_picture(rid, "<w:vanish w:val=\"0\"/>")],
            )]
        });
        let e = to_email_html(&pkg.document, Some(&pkg));
        assert_eq!((e.html.matches("<img").count(), e.media.len()), (1, 1));
    }

    #[test]
    fn a_style_hidden_picture_is_not_written() {
        let mut pkg = with_picture(&png(4, 4), |rid| {
            vec![para(
                ParProps::default(),
                vec![
                    plain("text "),
                    hidden_picture(rid, "<w:rStyle w:val=\"Secret\"/>"),
                ],
            )]
        });
        let styles = pkg.part_text("word/styles.xml").expect("styles");
        let styles = styles.replace(
            "</w:styles>",
            "<w:style w:type=\"character\" w:styleId=\"Secret\"><w:name w:val=\"Secret\"/>\
             <w:rPr><w:vanish/></w:rPr></w:style></w:styles>",
        );
        assert!(pkg.set_part_text("word/styles.xml", &styles));
        let e = to_email_html(&pkg.document, Some(&pkg));
        assert!(!e.html.contains("<img"), "{}", e.html);
        assert!(e.media.is_empty());
    }

    #[test]
    fn hidden_footnote_ref_and_text_box_are_not_written() {
        let hidden = |inner: &str| format!("<w:r><w:rPr><w:vanish/></w:rPr>{inner}</w:r>");
        let h = html(&doc(vec![para(
            ParProps::default(),
            vec![
                plain("shown"),
                Inline::FootnoteRef {
                    id: 7,
                    endnote: false,
                    raw: hidden("<w:footnoteReference w:id=\"7\"/>"),
                },
                Inline::TextBox {
                    raw: hidden("<w:drawing/>"),
                    blocks: vec![text_para("boxed")],
                },
                Inline::SmartArt {
                    raw: hidden("<w:drawing/>"),
                    text: vec!["diagram".into()],
                },
            ],
        )]));
        assert!(h.contains("shown"), "{h}");
        assert!(!h.contains("<sup>"), "{h}");
        assert!(!h.contains("boxed") && !h.contains("diagram"), "{h}");
    }
}
