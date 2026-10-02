//! RTF → [`Document`] (#633): the RTF Word writes (Save As Rich Text Format),
//! read with a group/control-word tokenizer.
//!
//! Kept: paragraphs, tabs, line and page breaks, bold / italic / underline /
//! strikethrough / superscript / subscript / small caps / all caps,
//! paragraph alignment, `heading N` paragraph styles (named in the
//! stylesheet), list membership (`\ls`, `\ilvl`, the marker in `\listtext`
//! saying bullet or number), tables (`\trowd … \cell … \row`), field results
//! (hyperlink text), `\uN` with its `\ucN` fallback skipped, and `\'hh`
//! decoded in the code page the font's `\fcharset` (or `\ansicpg`) names.
//!
//! Dropped: pictures, objects, headers / footers, notes, annotations, field
//! instructions, hidden text, fonts, sizes and colours. A double-byte code
//! page (`\ansicpg932`, 936, 949, 950) has no table here: each of its byte
//! pairs comes out as U+FFFD rather than failing the document.

use super::{Builder, heading_props, marker_is_numbered};
use crate::model::{Align, Document, ParProps, RunProps, VertAlign};
use std::collections::HashMap;

/// Read an RTF file. `Err` only when it is not RTF at all or holds no text.
pub fn import_rtf(bytes: &[u8]) -> Result<Document, String> {
    let start = bytes
        .iter()
        .position(|&b| b == b'{')
        .filter(|&i| bytes[i..].starts_with(b"{\\rtf"))
        .ok_or_else(|| "not an RTF file (no {\\rtf header)".to_string())?;
    let mut r = Reader::new(&bytes[start..]);
    r.run();
    if r.out.is_empty() {
        return Err("the RTF file holds no text".into());
    }
    let props = r.par_props();
    Ok(r.out.finish(props))
}

/// Where the text of the current group goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dest {
    Body,
    /// Dropped: an ignorable or unwanted destination.
    Skip,
    FontTable,
    StyleSheet,
    /// The list marker Word writes before a list paragraph.
    ListText,
}

#[derive(Clone, Debug)]
struct State {
    dest: Dest,
    run: RunProps,
    hidden: bool,
    uc: usize,
    font: i32,
    // Paragraph properties are group-scoped in RTF too.
    style: Option<i32>,
    align: Align,
    in_table: bool,
    list: Option<i32>,
    ilvl: i32,
    /// `\pnlvlblt` / `\pnlvlbody` (Word 95 lists): a list paragraph, bullet or
    /// numbered.
    pn: Option<bool>,
}

#[derive(Default, Clone)]
struct Font {
    charset: Option<i32>,
    codepage: Option<i32>,
}

struct Reader<'a> {
    src: &'a [u8],
    pos: usize,
    stack: Vec<State>,
    st: State,
    out: Builder,
    ansi_page: i32,
    default_font: i32,
    fonts: HashMap<i32, Font>,
    /// Paragraph style number → heading level (from stylesheet names).
    headings: HashMap<i32, u8>,
    /// The stylesheet entry being read: its `\sN` (paragraph styles only)
    /// and its name so far.
    style_entry: Option<(Option<i32>, String)>,
    style_depth: usize,
    font_entry: i32,
    /// `\uN` fallback bytes still to drop.
    skip: usize,
    /// A UTF-16 high surrogate waiting for its low half.
    high: Option<u16>,
    /// A double-byte lead byte waiting for its trail byte.
    lead: bool,
    list_text: String,
}

impl<'a> Reader<'a> {
    fn new(src: &'a [u8]) -> Self {
        Reader {
            src,
            pos: 0,
            stack: Vec::new(),
            st: State {
                dest: Dest::Body,
                run: RunProps::default(),
                hidden: false,
                uc: 1,
                font: 0,
                style: None,
                align: Align::Left,
                in_table: false,
                list: None,
                ilvl: 0,
                pn: None,
            },
            out: Builder::new(),
            ansi_page: 1252,
            default_font: 0,
            fonts: HashMap::new(),
            headings: HashMap::new(),
            style_entry: None,
            style_depth: 0,
            font_entry: 0,
            skip: 0,
            high: None,
            lead: false,
            list_text: String::new(),
        }
    }

    fn run(&mut self) {
        while self.pos < self.src.len() {
            let b = self.src[self.pos];
            self.pos += 1;
            match b {
                b'{' => self.open_group(),
                b'}' => {
                    if !self.close_group() {
                        return; // the closing brace of the document
                    }
                }
                b'\\' => self.control(),
                b'\r' | b'\n' => {}
                _ => self.byte(b),
            }
        }
    }

    fn open_group(&mut self) {
        self.stack.push(self.st.clone());
        // A stylesheet / font table entry starts with its group.
        if self.st.dest == Dest::StyleSheet && self.stack.len() == self.style_depth + 1 {
            self.style_entry = Some((None, String::new()));
        }
    }

    /// Pop a group; `false` when the outermost group closed.
    fn close_group(&mut self) -> bool {
        if self.st.dest == Dest::StyleSheet && self.stack.len() == self.style_depth + 1 {
            if let Some((Some(n), name)) = self.style_entry.take() {
                let name = name
                    .trim()
                    .trim_end_matches(';')
                    .trim()
                    .to_ascii_lowercase();
                if let Some(level) = name
                    .strip_prefix("heading")
                    .and_then(|r| r.trim().parse::<u8>().ok())
                {
                    self.headings.insert(n, level);
                }
            }
        }
        let Some(prev) = self.stack.pop() else {
            return false;
        };
        self.st = prev;
        self.skip = 0;
        !self.stack.is_empty()
    }

    fn control(&mut self) {
        let Some(&c) = self.src.get(self.pos) else {
            return;
        };
        if !c.is_ascii_alphabetic() {
            self.pos += 1;
            return self.symbol(c);
        }
        let start = self.pos;
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_alphabetic() {
            self.pos += 1;
        }
        let word = &self.src[start..self.pos];
        let mut num: Option<i32> = None;
        let neg = self.src.get(self.pos) == Some(&b'-');
        if neg {
            self.pos += 1;
        }
        let ds = self.pos;
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() && self.pos - ds < 10
        {
            self.pos += 1;
        }
        if self.pos > ds {
            let n: i64 = std::str::from_utf8(&self.src[ds..self.pos])
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            num = Some(if neg { -n } else { n }.clamp(i32::MIN as i64, i32::MAX as i64) as i32);
        } else if neg {
            self.pos -= 1; // a lone '-' is text
        }
        if self.src.get(self.pos) == Some(&b' ') {
            self.pos += 1;
        }
        let word = word.to_vec();
        self.word(&word, num);
    }

    fn symbol(&mut self, c: u8) {
        match c {
            b'\'' => {
                let hex = self.src.get(self.pos..self.pos + 2);
                let v = hex
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                if let Some(v) = v {
                    self.pos += 2;
                    self.hex_byte(v);
                }
            }
            b'\\' | b'{' | b'}' => self.byte(c),
            b'~' => self.text("\u{a0}"),
            b'_' => self.text("\u{2011}"),
            b'-' => {}
            b'*' => {
                // An ignorable destination: dropped unless it is one we read.
                if !self.next_is_known_destination() {
                    self.st.dest = Dest::Skip;
                }
            }
            b'\n' | b'\r' => self.par(),
            b'\t' => self.tab(),
            _ => {}
        }
    }

    /// After `\*`: whether the control word that follows is a destination we
    /// read rather than drop.
    fn next_is_known_destination(&self) -> bool {
        let rest = &self.src[self.pos.min(self.src.len())..];
        let rest = rest.strip_prefix(b"\\").unwrap_or(&[]);
        let end = rest
            .iter()
            .position(|b| !b.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        matches!(&rest[..end], b"ud" | b"fldinst" | b"listtext")
    }

    fn word(&mut self, w: &[u8], num: Option<i32>) {
        let n = num.unwrap_or(1);
        let on = num != Some(0);
        // Destinations first: they change where the group's text goes.
        match w {
            b"fonttbl" => {
                self.st.dest = Dest::FontTable;
                return;
            }
            b"stylesheet" => {
                self.st.dest = Dest::StyleSheet;
                self.style_depth = self.stack.len();
                return;
            }
            b"listtext" | b"pntext" => {
                self.st.dest = Dest::ListText;
                self.list_text.clear();
                return;
            }
            b"ud" => {
                // `{\upr{ansi}{\*\ud{unicode}}}`: the Unicode half is read.
                self.st.dest = Dest::Body;
                return;
            }
            b"upr"
            | b"colortbl"
            | b"info"
            | b"pict"
            | b"object"
            | b"header"
            | b"headerl"
            | b"headerr"
            | b"headerf"
            | b"footer"
            | b"footerl"
            | b"footerr"
            | b"footerf"
            | b"footnote"
            | b"annotation"
            | b"fldinst"
            | b"xe"
            | b"tc"
            | b"txe"
            | b"rxe"
            | b"bkmkstart"
            | b"bkmkend"
            | b"listtable"
            | b"listoverridetable"
            | b"revtbl"
            | b"rsidtbl"
            | b"generator"
            | b"themedata"
            | b"colorschememapping"
            | b"latentstyles"
            | b"datastore"
            | b"xmlnstbl"
            | b"filetbl"
            | b"pgdsctbl"
            | b"nonshppict"
            | b"shppict"
            | b"shp"
            | b"pntxta"
            | b"pntxtb"
            | b"docvar"
            | b"mmathPr"
            | b"background"
            | b"atnid"
            | b"atnauthor"
            | b"template"
            | b"nesttableprops"
            | b"fldtype"
            | b"formfield"
            | b"datafield"
            | b"wgrffmtfilter"
            | b"passwordhash"
            | b"ftnsep"
            | b"ftnsepc"
            | b"aftnsep"
            | b"aftnsepc" => {
                self.st.dest = Dest::Skip;
                return;
            }
            _ => {}
        }
        match self.st.dest {
            Dest::FontTable => return self.font_word(w, num),
            Dest::StyleSheet => {
                if self.stack.len() == self.style_depth + 1 {
                    match w {
                        b"s" => {
                            if let Some((n0, _)) = self.style_entry.as_mut() {
                                *n0 = Some(n);
                            }
                        }
                        b"cs" | b"ds" | b"ts" | b"tsrowd" => self.style_entry = None,
                        _ => {}
                    }
                }
                return;
            }
            Dest::Skip => return,
            Dest::Body | Dest::ListText => {}
        }
        match w {
            b"ansicpg" => self.ansi_page = n,
            b"deff" => {
                self.default_font = n;
                self.st.font = n;
            }
            b"uc" => self.st.uc = n.max(0) as usize,
            b"u" => self.unicode(n),
            b"par" | b"sect" => self.par(),
            b"line" => {
                if self.visible() {
                    self.out.line_break(&self.st.run);
                }
            }
            b"page" => {
                if self.visible() {
                    self.out.page_break(&self.st.run);
                }
            }
            b"tab" => self.tab(),
            b"cell" => {
                let p = self.par_props();
                self.out.end_cell(p);
            }
            b"row" => {
                let p = self.par_props();
                self.out.end_row(p);
            }
            b"nestcell" => self.tab(),
            b"emdash" => self.text("\u{2014}"),
            b"endash" => self.text("\u{2013}"),
            b"bullet" => self.text("\u{2022}"),
            b"lquote" => self.text("\u{2018}"),
            b"rquote" => self.text("\u{2019}"),
            b"ldblquote" => self.text("\u{201c}"),
            b"rdblquote" => self.text("\u{201d}"),
            b"emspace" => self.text("\u{2003}"),
            b"enspace" => self.text("\u{2002}"),
            b"qmspace" => self.text("\u{2005}"),
            b"plain" => {
                self.st.run = RunProps::default();
                self.st.hidden = false;
                self.st.font = self.default_font;
            }
            b"b" => self.st.run.bold = on,
            b"i" => self.st.run.italic = on,
            b"ul" | b"uld" | b"uldash" | b"uldashd" | b"uldashdd" | b"uldb" | b"ulth" | b"ulw"
            | b"ulwave" | b"ulhwave" | b"ululdbwave" | b"ulthd" | b"ulthdash" | b"ulldash"
            | b"ulthldash" | b"ulthdashd" | b"ulthdashdd" => self.st.run.underline = on,
            b"ulnone" => self.st.run.underline = false,
            b"strike" | b"striked" => self.st.run.strike = on,
            b"caps" => self.st.run.caps = on,
            b"scaps" => self.st.run.small_caps = on,
            b"super" => self.st.run.vert_align = VertAlign::Superscript,
            b"sub" => self.st.run.vert_align = VertAlign::Subscript,
            b"nosupersub" => self.st.run.vert_align = VertAlign::Baseline,
            b"v" => self.st.hidden = on,
            b"f" => self.st.font = n,
            b"pard" => {
                self.st.style = None;
                self.st.align = Align::Left;
                self.st.in_table = false;
                self.st.list = None;
                self.st.ilvl = 0;
                self.st.pn = None;
            }
            b"s" => self.st.style = Some(n),
            b"ql" => self.st.align = Align::Left,
            b"qc" => self.st.align = Align::Center,
            b"qr" => self.st.align = Align::Right,
            b"qj" => self.st.align = Align::Justify,
            b"intbl" => self.st.in_table = true,
            b"ls" => self.st.list = Some(n),
            b"ilvl" => self.st.ilvl = n.clamp(0, 8),
            b"pnlvlblt" => self.st.pn = Some(false),
            b"pnlvlbody" | b"pnlvlcont" => self.st.pn = Some(true),
            _ => {}
        }
    }

    fn font_word(&mut self, w: &[u8], num: Option<i32>) {
        let n = num.unwrap_or(0);
        match w {
            b"f" => {
                self.font_entry = n;
                self.fonts.entry(n).or_default();
            }
            b"fcharset" => self.fonts.entry(self.font_entry).or_default().charset = Some(n),
            b"cpg" => self.fonts.entry(self.font_entry).or_default().codepage = Some(n),
            _ => {}
        }
    }

    fn visible(&self) -> bool {
        self.st.dest == Dest::Body && !self.st.hidden
    }

    fn text(&mut self, s: &str) {
        // A high surrogate with no low half after it stands for nothing.
        let owned;
        let s = if self.high.take().is_some() {
            owned = format!("\u{fffd}{s}");
            owned.as_str()
        } else {
            s
        };
        match self.st.dest {
            Dest::Body if !self.st.hidden => self.out.text(s, &self.st.run),
            Dest::ListText => self.list_text.push_str(s),
            Dest::StyleSheet => {
                if let Some((_, name)) = self.style_entry.as_mut() {
                    name.push_str(s);
                }
            }
            _ => {}
        }
    }

    fn tab(&mut self) {
        match self.st.dest {
            Dest::Body if !self.st.hidden => self.out.tab(&self.st.run),
            Dest::ListText => self.list_text.push('\t'),
            _ => {}
        }
    }

    /// A literal byte of text (not an escape).
    fn byte(&mut self, b: u8) {
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        if b < 0x80 {
            self.lead = false;
            let mut buf = [0u8; 4];
            let s = char::from(b).encode_utf8(&mut buf).to_string();
            self.text(&s);
        } else {
            self.decoded(b);
        }
    }

    /// `\'hh`.
    fn hex_byte(&mut self, b: u8) {
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        self.decoded(b);
    }

    /// A byte in the current font's code page.
    fn decoded(&mut self, b: u8) {
        let page = self.page();
        if matches!(page, 932 | 936 | 949 | 950) {
            // A lead byte and its trail make one character we cannot map.
            if self.lead {
                self.lead = false;
                return;
            }
            if b >= 0x81 {
                self.lead = true;
                self.text("\u{fffd}");
                return;
            }
        }
        let c = if page == 42 {
            char::from_u32(0xF000 + u32::from(b))
        } else {
            opccore::codepage::decode(page, b)
        };
        let mut buf = [0u8; 4];
        let s = c.unwrap_or('\u{fffd}').encode_utf8(&mut buf).to_string();
        self.text(&s);
    }

    fn page(&self) -> i32 {
        let font = self.fonts.get(&self.st.font).cloned().unwrap_or_default();
        font.codepage
            .or_else(|| {
                font.charset
                    .and_then(|cs| opccore::codepage::charset_page(cs, self.ansi_page))
            })
            .unwrap_or(self.ansi_page)
    }

    fn unicode(&mut self, n: i32) {
        let unit = if n < 0 { (n + 65536) as u16 } else { n as u16 };
        if (0xD800..0xDC00).contains(&unit) {
            if self.high.is_some() {
                self.text(""); // the unpaired one before it
            }
            self.high = Some(unit);
            self.skip = self.st.uc;
            return;
        }
        let c = if (0xDC00..0xE000).contains(&unit) {
            match self.high.take() {
                Some(h) => char::decode_utf16([h, unit]).next().and_then(|r| r.ok()),
                None => None,
            }
        } else {
            char::from_u32(u32::from(unit))
        };
        let mut buf = [0u8; 4];
        let s = c.unwrap_or('\u{fffd}').encode_utf8(&mut buf).to_string();
        // `text` flushes a pending high surrogate first; it was taken above.
        self.text(&s);
        // `\uN` is followed by its fallback: `skip` drops it.
        self.skip = self.st.uc;
    }

    fn par_props(&self) -> ParProps {
        let mut p = match self.st.style.and_then(|s| self.headings.get(&s)) {
            Some(&level) => heading_props(level),
            None => ParProps::default(),
        };
        p.align = self.st.align;
        let listed = self.st.list.is_some_and(|l| l > 0) || self.st.pn.is_some();
        if listed && p.heading_level.is_none() {
            let numbered = match self.st.pn {
                Some(n) if self.list_text.is_empty() => n,
                _ => marker_is_numbered(&self.list_text),
            };
            p.num_id = Some(if numbered { 2 } else { 1 });
            p.ilvl = self.st.ilvl;
        }
        p
    }

    fn par(&mut self) {
        if self.st.dest != Dest::Body {
            return;
        }
        let p = self.par_props();
        self.out.end_para(p, self.st.in_table);
        self.list_text.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::paragraph_texts;
    use crate::model::{Block, Inline};

    /// `\U` spelled for `\u`: RTF's Unicode control word, written so that no
    /// tool on the way mistakes `\u` and four hex digits for an escape.
    fn u(rtf: &str) -> String {
        rtf.replace(r"\U", r"\u")
    }

    fn texts(rtf: &str) -> Vec<String> {
        paragraph_texts(&import_rtf(rtf.as_bytes()).unwrap())
    }

    fn paras(doc: &Document) -> Vec<&crate::model::Paragraph> {
        doc.body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn paragraphs_tabs_and_breaks() {
        let doc = import_rtf(br"{\rtf1\ansi Hello\tab World\par Second\line line\par}").unwrap();
        assert_eq!(paragraph_texts(&doc), ["Hello\tWorld", "Second\nline"]);
        let p = paras(&doc);
        assert!(matches!(p[0].content[1], Inline::Tab(_)));
        assert!(matches!(p[1].content[1], Inline::Break(..)));
    }

    #[test]
    fn character_formatting_is_group_scoped() {
        let doc = import_rtf(
            br"{\rtf1 Plain {\b bold} {\i italic} {\ul under}{\ulnone  no}\b B\b0  end\par}",
        )
        .unwrap();
        let p = paras(&doc);
        let runs: Vec<(String, bool, bool, bool)> = p[0]
            .content
            .iter()
            .filter_map(|i| match i {
                Inline::Run(r) => Some((
                    r.text.clone(),
                    r.props.bold,
                    r.props.italic,
                    r.props.underline,
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            runs,
            [
                ("Plain ".into(), false, false, false),
                ("bold".into(), true, false, false),
                (" ".into(), false, false, false),
                ("italic".into(), false, true, false),
                (" ".into(), false, false, false),
                ("under".into(), false, false, true),
                (" no".into(), false, false, false),
                ("B".into(), true, false, false),
                (" end".into(), false, false, false),
            ]
        );
    }

    #[test]
    fn code_pages_and_unicode() {
        // \'e9 in 1252; \'c6 in a Cyrillic (charset 204) font; \U1046 with
        // its one-byte fallback skipped; a surrogate pair.
        let rtf = u(
            r"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0 Times;}{\f1\fcharset204 Arial Cyr;}}Caf\'e9 {\f1\'c6} \uc1\U1046?\u-10179?\u-8704?\par}",
        );
        assert_eq!(texts(&rtf), ["Caf\u{e9} \u{416} \u{416}\u{1f600}"]);
        // \uc2 skips two fallback bytes, an escape counting as one.
        assert_eq!(texts(&u(r"{\rtf1\uc2\U1046\'3f\'3fx\par}")), ["\u{416}x"]);
        assert_eq!(texts(&u(r"{\rtf1\uc0\U1046 x\par}")), ["\u{416}x"]);
    }

    #[test]
    fn double_byte_pages_give_one_replacement_per_pair() {
        assert_eq!(
            texts(r"{\rtf1\ansi\ansicpg932 a\'82\'a0b\par}"),
            ["a\u{fffd}b"]
        );
    }

    #[test]
    fn destinations_are_dropped_and_field_results_kept() {
        let rtf = r"{\rtf1{\info{\author someone}}{\colortbl;\red0\green0\blue0;}{\*\generator Riched20;}{\header head}{\*\unknown dropped}A{\field{\*\fldinst HYPERLINK x}{\fldrslt link}}B{\pict 0102}{\v hidden}C{\upr{ansi}{\*\ud{uni}}}\par}";
        assert_eq!(texts(rtf), ["AlinkBCuni"]);
    }

    #[test]
    fn heading_styles_come_from_the_stylesheet() {
        let rtf = r"{\rtf1{\stylesheet{\ql Normal;}{\s1\ql\outlinelevel0 heading 1;}{\s2 heading 2;}{\*\cs10 Default Paragraph Font;}}\pard\s1 Title\par\pard\s2 Sub\par\pard Body\par}";
        let doc = import_rtf(rtf.as_bytes()).unwrap();
        let p = paras(&doc);
        assert_eq!(p[0].props.style_id.as_deref(), Some("Heading1"));
        assert_eq!(p[0].props.heading_level, Some(1));
        assert_eq!(p[1].props.style_id.as_deref(), Some("Heading2"));
        assert_eq!(p[2].props.style_id, None);
        assert_eq!(paragraph_texts(&doc), ["Title", "Sub", "Body"]);
    }

    #[test]
    fn list_markers_set_the_list_and_are_not_text() {
        let rtf = r"{\rtf1{\*\listtable{\list x}}\pard\ls1\ilvl0{\listtext\f3\'b7\tab}Apples\par{\listtext\f3\'b7\tab}Bananas\par\pard\ls2{\listtext 1.\tab}First\par\pard Plain\par}";
        let doc = import_rtf(rtf.as_bytes()).unwrap();
        assert_eq!(
            paragraph_texts(&doc),
            ["Apples", "Bananas", "First", "Plain"]
        );
        let p = paras(&doc);
        assert_eq!(p[0].props.num_id, Some(1));
        assert_eq!(p[1].props.num_id, Some(1));
        assert_eq!(p[2].props.num_id, Some(2));
        assert_eq!(p[3].props.num_id, None);
    }

    #[test]
    fn tables_from_cells_and_rows() {
        // Word repeats the row definition after the cells, before \row.
        let rtf = r"{\rtf1\pard Before\par\trowd\cellx3000\cellx6000\pard\intbl{North\cell}{South\cell}\pard\intbl{\trowd\cellx3000\cellx6000\row}\trowd\cellx3000\cellx6000\pard\intbl 10\cell 20\cell\row\pard After\par}";
        let doc = import_rtf(rtf.as_bytes()).unwrap();
        assert_eq!(
            paragraph_texts(&doc),
            ["Before", "North", "South", "10", "20", "After"]
        );
        let Block::Table(t) = &doc.body[1] else {
            panic!("{:?}", doc.body)
        };
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].cells.len(), 2);
    }

    #[test]
    fn not_rtf_or_empty_is_an_error() {
        assert!(import_rtf(b"hello").is_err());
        assert!(import_rtf(br"{\rtf1{\info{\title x}}}").is_err());
    }

    #[test]
    fn unbalanced_or_cut_input_never_panics() {
        let rtf = u(
            r"{\rtf1\ansi{\fonttbl{\f0\fcharset204 A;}}\f0\'c6\U1046?\U-10179{\b x\par}\trowd\cell\row\'",
        );
        let rtf = rtf.as_bytes();
        for n in 0..=rtf.len() {
            let _ = import_rtf(&rtf[..n]);
        }
        let _ = import_rtf(u(r"{\rtf1 }}}}} text \U99999999999 \'zz \uc-5 x").as_bytes());
    }
}
