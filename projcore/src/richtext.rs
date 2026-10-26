//! The portable value of a UI plugin's `richtext` dialog control (#1557): a
//! small HTML subset, bold, italic, paragraph alignment and bullets.
//!
//! One block per line, lines joined by `\n`:
//!
//! - `<p>`, `<p align="center">`, `<p align="right">`: a paragraph (left is
//!   the default and writes no attribute);
//! - `<li>`, `<li align="center">`, `<li align="right">`: a bulleted paragraph;
//! - inside a block `<b>`, `<i>` (written `<b><i>…</i></b>` when both) and the
//!   escapes `&amp; &lt; &gt;`; a `\n` never occurs inside a block.
//!
//! The writer is canonical (equal content, equal bytes): no whitespace between
//! blocks but the `\n`, adjacent runs of one style merged, an empty line
//! `<p></p>`. The reader is lenient and never fails: a value is markup only
//! when it starts with `<p` or `<li` followed by `>` or a space; anything else
//! (including `if a<b>c`) is plain text taken verbatim, one paragraph per
//! line. Inside markup unknown tags are dropped and their text kept, an
//! unknown or unclosed entity and a lone `<` are literal, `<br>` is ignored
//! and an `align` other than `center` or `right` means left.
//!
//! The model keeps the plain text apart from its attributes (one style byte
//! per character, one [`Para`] per `\n`-separated line), the shape an editor
//! edits in place. `projcore` stays dependency-free: a plugin that needs RTF
//! converts this markup itself.

/// Style bit of a bold character.
pub const BOLD: u8 = 1;
/// Style bit of an italic character.
pub const ITALIC: u8 = 2;

/// A paragraph's alignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

impl Align {
    /// The `align` attribute value, `None` for left (written as no attribute).
    fn attr(self) -> Option<&'static str> {
        match self {
            Self::Left => None,
            Self::Center => Some("center"),
            Self::Right => Some("right"),
        }
    }

    /// The `align` attribute's meaning: anything but center or right is left.
    fn from_attr(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "center" => Self::Center,
            "right" => Self::Right,
            _ => Self::Left,
        }
    }
}

/// One paragraph's properties.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Para {
    pub align: Align,
    pub bullet: bool,
}

/// Rich text: `text` with a style byte per character and a [`Para`] per
/// `\n`-separated line. `chars.len() == text.chars().count()` and
/// `paras.len() == text.split('\n').count()` (an empty text is one line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rich {
    pub text: String,
    pub chars: Vec<u8>,
    pub paras: Vec<Para>,
}

impl Default for Rich {
    fn default() -> Self {
        Self::plain("")
    }
}

/// Whether `value` is markup (starts with a `<p` or `<li` tag) rather than
/// plain text.
pub fn is_markup(value: &str) -> bool {
    let tag = |name: &str| {
        value
            .strip_prefix('<')
            .and_then(|r| r.strip_prefix(name))
            .and_then(|r| r.chars().next())
            .is_some_and(|c| c == '>' || c == ' ')
    };
    tag("p") || tag("li")
}

impl Rich {
    /// Plain text: no styles, left aligned paragraphs, one per line (`\r` is
    /// dropped).
    pub fn plain(text: &str) -> Self {
        let text: String = text.chars().filter(|&c| c != '\r').collect();
        Self {
            chars: vec![0; text.chars().count()],
            paras: vec![Para::default(); text.split('\n').count()],
            text,
        }
    }

    /// Read a value: markup when [`is_markup`], else plain text. Never fails.
    pub fn parse(value: &str) -> Self {
        if is_markup(value) {
            parse_markup(value)
        } else {
            Self::plain(value)
        }
    }

    /// The canonical markup.
    pub fn to_markup(&self) -> String {
        to_markup(&self.text, &self.chars, &self.paras)
    }

    /// The text without markup.
    pub fn to_plain(&self) -> String {
        self.text.clone()
    }
}

/// A task's notes from a rich value (#1801): the plain text (`None` when
/// empty) and the canonical markup, `None` unless it carries formatting the
/// plain text cannot (bold, italic, a non-left alignment or a bullet). A
/// value that is not markup is plain text, newline-normalised; empty plain
/// text keeps no markup (an empty bullet is no note).
pub fn split_notes(value: &str) -> (Option<String>, Option<String>) {
    if !is_markup(value) {
        let plain = crate::text::normalize_newlines(value);
        return ((!plain.is_empty()).then_some(plain), None);
    }
    let rich = parse_markup(value);
    if rich.text.is_empty() {
        return (None, None);
    }
    let markup = rich.to_markup();
    let formatted = markup != Rich::plain(&rich.text).to_markup();
    (Some(rich.text), formatted.then_some(markup))
}

/// The canonical markup of `text` with its attributes. Short or missing
/// attributes are read as plain / left, so a desynced state still writes.
pub fn to_markup(text: &str, chars: &[u8], paras: &[Para]) -> String {
    let mut out = String::new();
    let mut at = 0;
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let para = paras.get(n).copied().unwrap_or_default();
        let tag = if para.bullet { "li" } else { "p" };
        out.push('<');
        out.push_str(tag);
        if let Some(a) = para.align.attr() {
            out.push_str(" align=\"");
            out.push_str(a);
            out.push('"');
        }
        out.push('>');
        // Runs of one style, merged.
        let mut run = String::new();
        let mut run_style = 0u8;
        let flush = |out: &mut String, run: &mut String, style: u8| {
            if run.is_empty() {
                return;
            }
            if style & BOLD != 0 {
                out.push_str("<b>");
            }
            if style & ITALIC != 0 {
                out.push_str("<i>");
            }
            out.push_str(run);
            if style & ITALIC != 0 {
                out.push_str("</i>");
            }
            if style & BOLD != 0 {
                out.push_str("</b>");
            }
            run.clear();
        };
        for ch in line.chars() {
            let style = chars.get(at).copied().unwrap_or(0) & (BOLD | ITALIC);
            at += 1;
            if style != run_style {
                flush(&mut out, &mut run, run_style);
                run_style = style;
            }
            match ch {
                '&' => run.push_str("&amp;"),
                '<' => run.push_str("&lt;"),
                '>' => run.push_str("&gt;"),
                c => run.push(c),
            }
        }
        flush(&mut out, &mut run, run_style);
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
        at += 1; // the '\n' separator has a (plain) style byte too
    }
    out
}

/// A block being read.
struct Block {
    para: Para,
    chars: Vec<(char, u8)>,
}

fn parse_markup(src: &str) -> Rich {
    let src: Vec<char> = src.chars().filter(|&c| c != '\r').collect();
    let mut blocks: Vec<Block> = Vec::new();
    let mut open = false;
    let mut implicit = false;
    let (mut bold, mut italic) = (0usize, 0usize);
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        let tag_starts = |k: usize| src.get(k).is_some_and(|c| c.is_ascii_alphabetic());
        if c == '<' && (tag_starts(i + 1) || (src.get(i + 1) == Some(&'/') && tag_starts(i + 2))) {
            if let Some(end) = src[i + 1..].iter().position(|&c| c == '>') {
                let tag: String = src[i + 1..i + 1 + end].iter().collect();
                i += end + 2;
                let closing = tag.starts_with('/');
                let body = tag.trim_start_matches('/').trim_end_matches('/');
                let name = body
                    .split(|c: char| c.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match (name.as_str(), closing) {
                    ("p" | "li", false) => {
                        blocks.push(Block {
                            para: Para {
                                align: attr(body, "align")
                                    .map(|v| Align::from_attr(&v))
                                    .unwrap_or_default(),
                                bullet: name == "li",
                            },
                            chars: Vec::new(),
                        });
                        open = true;
                        implicit = false;
                        (bold, italic) = (0, 0);
                    }
                    ("p" | "li", true) => {
                        open = false;
                        (bold, italic) = (0, 0);
                    }
                    ("b", false) => bold += 1,
                    ("b", true) => bold = bold.saturating_sub(1),
                    ("i", false) => italic += 1,
                    ("i", true) => italic = italic.saturating_sub(1),
                    _ => {}
                }
                continue;
            }
            // A lone '<' is text.
        }
        i += 1;
        let ch = if c == '&' {
            let rest: String = src[i..].iter().take(4).collect();
            let known = [("amp;", '&'), ("lt;", '<'), ("gt;", '>')]
                .into_iter()
                .find(|(e, _)| rest.starts_with(e));
            match known {
                Some((e, ch)) => {
                    i += e.len();
                    ch
                }
                None => '&',
            }
        } else {
            c
        };
        if ch.is_whitespace() && !open {
            // The separator and other whitespace between blocks is layout.
            continue;
        }
        if !open {
            // Text outside any block: an implicit left paragraph, ended by
            // the line.
            blocks.push(Block {
                para: Para::default(),
                chars: Vec::new(),
            });
            open = true;
            implicit = true;
        }
        if ch == '\n' && implicit {
            open = false;
            implicit = false;
            continue;
        }
        if ch == '\n' {
            // A newline inside a block splits it, keeping its properties.
            let para = blocks.last().map(|b| b.para).unwrap_or_default();
            blocks.push(Block {
                para,
                chars: Vec::new(),
            });
            continue;
        }
        let style = if bold > 0 { BOLD } else { 0 } | if italic > 0 { ITALIC } else { 0 };
        if let Some(b) = blocks.last_mut() {
            b.chars.push((ch, style));
        }
    }
    if blocks.is_empty() {
        return Rich::plain("");
    }
    let mut out = Rich {
        text: String::new(),
        chars: Vec::new(),
        paras: Vec::new(),
    };
    for (n, b) in blocks.into_iter().enumerate() {
        if n > 0 {
            out.text.push('\n');
            out.chars.push(0);
        }
        out.paras.push(b.para);
        for (ch, style) in b.chars {
            out.text.push(ch);
            out.chars.push(style);
        }
    }
    out
}

/// The value of attribute `name` in a tag body (`p align="center"`), quoted
/// either way or bare.
fn attr(body: &str, name: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name) {
        let start = from + at;
        from = start + name.len();
        let before_ok = start == 0 || lower[..start].ends_with(char::is_whitespace);
        let rest = body[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=').filter(|_| before_ok) else {
            continue;
        };
        let rest = rest.trim_start();
        return Some(match rest.chars().next() {
            Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or("").to_string(),
            _ => rest
                .split(char::is_whitespace)
                .next()
                .unwrap_or("")
                .to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(text: &str, chars: &[u8], paras: &[Para]) -> Rich {
        Rich {
            text: text.into(),
            chars: chars.to_vec(),
            paras: paras.to_vec(),
        }
    }

    fn invariant(r: &Rich) {
        assert_eq!(r.chars.len(), r.text.chars().count(), "{r:?}");
        assert_eq!(r.paras.len(), r.text.split('\n').count(), "{r:?}");
    }

    #[test]
    fn plain_text_is_a_paragraph_per_line() {
        let r = Rich::parse("one\ntwo\n\nfour");
        assert_eq!(r.text, "one\ntwo\n\nfour");
        assert_eq!(r.paras.len(), 4);
        assert!(r.chars.iter().all(|&s| s == 0));
        invariant(&r);
        assert_eq!(
            r.to_markup(),
            "<p>one</p>\n<p>two</p>\n<p></p>\n<p>four</p>"
        );
        let empty = Rich::parse("");
        assert_eq!(empty.to_markup(), "<p></p>");
        invariant(&empty);
    }

    #[test]
    fn markup_is_recognised_by_its_shape_only() {
        assert!(is_markup("<p>x</p>"));
        assert!(is_markup("<li align=\"right\">x</li>"));
        assert!(!is_markup("if a<b>c"));
        assert!(!is_markup("<para>x"));
        assert!(!is_markup("<pre>x</pre>"));
        assert!(!is_markup(" <p>x</p>"));
        assert!(!is_markup(""));
        // Not markup: kept verbatim, tags and all.
        let r = Rich::parse("if a<b>c");
        assert_eq!(r.text, "if a<b>c");
        assert_eq!(r.to_markup(), "<p>if a&lt;b&gt;c</p>");
        assert_eq!(Rich::parse("<para>x").text, "<para>x");
    }

    #[test]
    fn escaping_survives_a_round_trip() {
        let r = Rich::parse("a < b & c > d &amp; &lt;");
        let markup = r.to_markup();
        assert_eq!(markup, "<p>a &lt; b &amp; c &gt; d &amp;amp; &amp;lt;</p>");
        assert_eq!(Rich::parse(&markup), r);
    }

    #[test]
    fn each_attribute_round_trips() {
        let cases = [
            styled("bold", &[BOLD; 4], &[Para::default()]),
            styled("it", &[ITALIC; 2], &[Para::default()]),
            styled("both", &[BOLD | ITALIC; 4], &[Para::default()]),
            styled(
                "ab",
                &[0, BOLD],
                &[Para {
                    align: Align::Center,
                    bullet: false,
                }],
            ),
            styled(
                "x\ny",
                &[0, 0, 0],
                &[
                    Para {
                        align: Align::Right,
                        bullet: true,
                    },
                    Para {
                        align: Align::Left,
                        bullet: true,
                    },
                ],
            ),
            styled(
                "a < b\n\nc & d",
                &[BOLD, BOLD, 0, 0, 0, 0, 0, ITALIC, 0, 0, 0, 0],
                &[Para::default(); 3],
            ),
        ];
        for r in cases {
            invariant(&r);
            let markup = r.to_markup();
            let back = Rich::parse(&markup);
            assert_eq!(back, r, "{markup}");
            assert_eq!(back.to_markup(), markup);
        }
    }

    #[test]
    fn the_writer_is_canonical() {
        let r = styled(
            "abcd",
            &[BOLD, BOLD, BOLD | ITALIC, 0],
            &[Para {
                align: Align::Right,
                bullet: true,
            }],
        );
        assert_eq!(
            r.to_markup(),
            "<li align=\"right\"><b>ab</b><b><i>c</i></b>d</li>"
        );
        // Different spellings of the same content write the same bytes.
        let loose = "<li align='right'><b>a</b><b>b</b><i><b>c</b></i>d</li>";
        assert_eq!(Rich::parse(loose).to_markup(), r.to_markup());
        // The writer survives attributes shorter than the text.
        assert_eq!(
            to_markup("ab\ncd", &[BOLD], &[]),
            "<p><b>a</b>b</p>\n<p>cd</p>"
        );
    }

    #[test]
    fn the_reader_is_lenient() {
        // Unknown tags are dropped, their text kept.
        let r = Rich::parse("<p>a<u>b</u><br>c</p>");
        assert_eq!(r.text, "abc");
        // Unknown and unclosed entities and a lone '<' are literal.
        let r = Rich::parse("<p>&nbsp; &amp x < y &</p>");
        assert_eq!(r.text, "&nbsp; &amp x < y &");
        // An unclosed block and unbalanced closers do not fail.
        let r = Rich::parse("<p><b>x");
        assert_eq!((r.text.as_str(), r.chars.as_slice()), ("x", &[BOLD][..]));
        let r = Rich::parse("<p></b></i>x</p>");
        assert_eq!(r.chars, vec![0]);
        // An unknown align is left; case is ignored.
        let r = Rich::parse("<p align=\"justify\">a</p>\n<P ALIGN=CENTER>b</P>");
        assert_eq!(r.paras[0].align, Align::Left);
        assert_eq!(r.text, "a\nb");
        // Styles do not leak into the next block.
        let r = Rich::parse("<p><b>a</p>\n<p>b</p>");
        assert_eq!(r.chars, vec![BOLD, 0, 0]);
        // ... nor into text outside any block.
        let r = Rich::parse("<p><i>a</p>\ntail");
        assert_eq!(r.chars, vec![ITALIC, 0, 0, 0, 0, 0]);
        // Text outside blocks becomes a paragraph; layout whitespace does not.
        let r = Rich::parse("<p>a</p>\n  tail\n");
        assert_eq!(r.text, "a\ntail");
        invariant(&r);
        // A newline inside a block splits it, keeping its properties.
        let r = Rich::parse("<li>a\nb</li>");
        assert_eq!(r.text, "a\nb");
        assert!(r.paras.iter().all(|p| p.bullet));
        // Markup with no block text at all is one empty line.
        let r = Rich::parse("<p></p>");
        assert_eq!(r, Rich::plain(""));
        for s in ["<p", "<p>", "<li>&", "<p><", "<p>&amp", "<p a='>x</p>"] {
            invariant(&Rich::parse(s));
        }
    }

    #[test]
    fn split_notes_keeps_markup_only_when_it_formats() {
        // Plain text, newline-normalised, no markup.
        assert_eq!(split_notes("a\r\nb"), (Some("a\nb".into()), None));
        assert_eq!(split_notes(""), (None, None));
        // Markup without formatting is its plain text.
        assert_eq!(
            split_notes("<p> a\tb </p>\n<p></p>\n<p>c</p>"),
            (Some(" a\tb \n\nc".into()), None)
        );
        // A plain note that looks like a tag is escaped, so it formats nothing.
        let looks = Rich::plain("<p>hi").to_markup();
        assert_eq!(looks, "<p>&lt;p&gt;hi</p>");
        assert_eq!(split_notes(&looks), (Some("<p>hi".into()), None));
        // Bold, a bullet or an alignment keep the canonical markup.
        for m in [
            "<p><b>x</b> y</p>",
            "<li>x</li>",
            "<p align=\"center\">x</p>",
            "<p><i>a &amp; &lt;b&gt;</i></p>\n<li align=\"right\">c</li>",
        ] {
            let (plain, rich) = split_notes(m);
            assert_eq!(rich.as_deref(), Some(m), "{m}");
            assert_eq!(plain, Some(Rich::parse(m).to_plain()));
        }
        // Not canonical in, canonical out.
        assert_eq!(
            split_notes("<p><b>x</b><b>y</b></p>").1.as_deref(),
            Some("<p><b>xy</b></p>")
        );
        // An empty formatted line is no note.
        assert_eq!(split_notes("<li></li>"), (None, None));
    }
}
