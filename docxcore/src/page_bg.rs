//! Word's page colour (`w:background`, Design > Page Color, #651).
//!
//! The body model has no place for `w:background`: save carries the raw XML
//! between `<w:document>` and `<w:body>` over from the document part (see
//! [`crate::package::save_package`]). So the page colour is read from and
//! written into that part's text directly, and only that one element is
//! touched. [`crate::package::Package::set_page_background`] wraps these with
//! the `w:displayBackgroundShape` settings flag Word needs to show it.

use crate::xml::{Event, XmlParser};

pub(crate) const VML_NS: &str = "urn:schemas-microsoft-com:vml";
pub(crate) const OFFICE_NS: &str = "urn:schemas-microsoft-com:office:office";

/// A page colour: a solid `w:color`, optionally with a Fill Effects gradient
/// (`v:background`/`v:fill`) whose first colour is `color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageBackground {
    /// `w:color`, RRGGBB.
    pub color: u32,
    pub gradient: Option<Gradient>,
}

/// A Fill Effects gradient from [`PageBackground::color`] to `color2`.
///
/// Word writes a one-colour gradient's second colour as an expression
/// (`fill darken(118)`); this writes an explicit colour (white for one
/// colour) and reads an expression back as white.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gradient {
    pub color2: u32,
    pub style: GradientStyle,
}

/// The Fill Effects Gradient tab's Shading styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GradientStyle {
    #[default]
    Horizontal,
    Vertical,
    DiagonalUp,
    DiagonalDown,
    FromCenter,
}

impl GradientStyle {
    pub const ALL: [GradientStyle; 5] = [
        Self::Horizontal,
        Self::Vertical,
        Self::DiagonalUp,
        Self::DiagonalDown,
        Self::FromCenter,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Horizontal => "Horizontal",
            Self::Vertical => "Vertical",
            Self::DiagonalUp => "Diagonal up",
            Self::DiagonalDown => "Diagonal down",
            Self::FromCenter => "From center",
        }
    }

    /// The `v:fill` attributes (besides `color2`) Word writes for the style.
    fn fill_attrs(self) -> &'static str {
        match self {
            Self::Horizontal => " type=\"gradient\"",
            Self::Vertical => " angle=\"-90\" type=\"gradient\"",
            Self::DiagonalUp => " angle=\"-45\" type=\"gradient\"",
            Self::DiagonalDown => " angle=\"-135\" type=\"gradient\"",
            Self::FromCenter => " focusposition=\".5,.5\" focussize=\"\" type=\"gradientRadial\"",
        }
    }
}

/// The byte range of the `w:background` element in a document part, only
/// between the root's start tag and `<w:body>`.
fn background_span(doc: &str) -> Option<(usize, usize)> {
    let mut parser = XmlParser::new(doc);
    let mut depth = 0usize;
    let mut start = None;
    loop {
        match parser.next() {
            Event::Start => {
                depth += 1;
                match (depth, parser.name()) {
                    (2, "w:background") => start = Some(parser.start_pos()),
                    (2, "w:body") => return None,
                    _ => {}
                }
            }
            Event::End => {
                if depth == 2 && parser.name() == "w:background" {
                    return Some((start?, parser.pos()));
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => return None,
            Event::Text => {}
        }
    }
}

fn hex(value: &str) -> Option<u32> {
    let v = value.trim().trim_start_matches('#');
    (v.len() == 6)
        .then(|| u32::from_str_radix(v, 16).ok())
        .flatten()
}

/// The page colour in a document part's XML; `None` without a
/// `w:background`, or when its `w:color` is absent or `auto`.
pub fn page_background(doc: &str) -> Option<PageBackground> {
    let (a, b) = background_span(doc)?;
    let el = &doc[a..b];
    let mut parser = XmlParser::new(el);
    let mut color = None;
    let mut gradient = None;
    loop {
        match parser.next() {
            Event::Start if parser.name() == "w:background" => {
                color = hex(parser.attr("w:color"));
            }
            Event::Start if parser.name() == "v:fill" => {
                let ty = parser.attr("type");
                if ty.starts_with("gradient") {
                    let angle: f32 = parser.attr("angle").trim().parse().unwrap_or(0.0);
                    let style = if ty == "gradientRadial" {
                        GradientStyle::FromCenter
                    } else if (angle + 90.0).abs() < 1.0 || (angle - 270.0).abs() < 1.0 {
                        GradientStyle::Vertical
                    } else if (angle + 45.0).abs() < 1.0 || (angle - 315.0).abs() < 1.0 {
                        GradientStyle::DiagonalUp
                    } else if (angle + 135.0).abs() < 1.0 || (angle - 225.0).abs() < 1.0 {
                        GradientStyle::DiagonalDown
                    } else {
                        GradientStyle::Horizontal
                    };
                    gradient = Some(Gradient {
                        color2: hex(parser.attr("color2")).unwrap_or(0xFFFFFF),
                        style,
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Some(PageBackground {
        color: color?,
        gradient,
    })
}

/// The `w:background` element Word writes for `bg`.
pub fn background_xml(bg: &PageBackground) -> String {
    let color = format!("{:06X}", bg.color & 0xFF_FFFF);
    match bg.gradient {
        None => format!("<w:background w:color=\"{color}\"/>"),
        Some(g) => format!(
            "<w:background w:color=\"{color}\"><v:background id=\"_x0000_s1025\" \
             o:bwmode=\"white\" o:targetscreensize=\"1024,768\"><v:fill color2=\"#{:06X}\"{}/>\
             </v:background></w:background>",
            g.color2 & 0xFF_FFFF,
            g.style.fill_attrs()
        ),
    }
}

/// Replace, insert (as the root's first child) or remove (`None`) the
/// document part's `w:background`, leaving every other byte alone. A gradient
/// also declares the `v`/`o` prefixes on the root when they are missing.
pub fn set_page_background(doc: &str, bg: Option<&PageBackground>) -> String {
    let mut out = match background_span(doc) {
        Some((a, b)) => format!("{}{}", &doc[..a], &doc[b..]),
        None => doc.to_string(),
    };
    let Some(bg) = bg else {
        return out;
    };
    if bg.gradient.is_some() {
        out = ensure_root_namespaces(&out, "w:document", &[("v", VML_NS), ("o", OFFICE_NS)]);
    }
    let Some(gt) = root_start_end(&out, "w:document") else {
        return out;
    };
    out.insert_str(gt + 1, &background_xml(bg));
    out
}

/// The index of the `>` closing `root`'s start tag (not a self-closing one).
fn root_start_end(xml: &str, root: &str) -> Option<usize> {
    let mut parser = XmlParser::new(xml);
    loop {
        match parser.next() {
            Event::Start if parser.name() == root => {
                let gt = parser.pos().checked_sub(1)?;
                return (!xml[..gt].ends_with('/')).then_some(gt);
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// Declare each `xmlns:prefix="uri"` the `root` start tag lacks, appended to
/// its attributes; a prefix already declared (to any URI) is left as it is.
pub(crate) fn ensure_root_namespaces(xml: &str, root: &str, ns: &[(&str, &str)]) -> String {
    let mut parser = XmlParser::new(xml);
    loop {
        match parser.next() {
            Event::Start if parser.name() == root => break,
            Event::Eof => return xml.to_string(),
            _ => {}
        }
    }
    let declared: Vec<&str> = parser
        .namespace_attrs()
        .iter()
        .chain(parser.attrs())
        .map(|a| a.name)
        .collect();
    let mut add = String::new();
    for (prefix, uri) in ns {
        let name = format!("xmlns:{prefix}");
        if !declared.contains(&name.as_str()) {
            add.push_str(&format!(" {name}=\"{uri}\""));
        }
    }
    if add.is_empty() {
        return xml.to_string();
    }
    let Some(gt) = parser.pos().checked_sub(1) else {
        return xml.to_string();
    };
    let at = if xml[..gt].ends_with('/') { gt - 1 } else { gt };
    format!("{}{add}{}", &xml[..at], &xml[at..])
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "<?xml version=\"1.0\"?><w:document xmlns:w=\"W\">";
    const BODY: &str = "<w:body><w:p/></w:body></w:document>";

    fn doc(prolog: &str) -> String {
        format!("{ROOT}{prolog}{BODY}")
    }

    #[test]
    fn solid_colour_is_inserted_replaced_and_removed() {
        let bare = doc("");
        assert_eq!(page_background(&bare), None);
        let red = PageBackground {
            color: 0xFF0000,
            gradient: None,
        };
        let out = set_page_background(&bare, Some(&red));
        assert_eq!(out, doc("<w:background w:color=\"FF0000\"/>"));
        assert_eq!(page_background(&out), Some(red));
        let blue = PageBackground {
            color: 0x0070C0,
            ..red
        };
        let out = set_page_background(&out, Some(&blue));
        assert_eq!(out, doc("<w:background w:color=\"0070C0\"/>"));
        assert_eq!(set_page_background(&out, None), bare);
        // A solid colour adds no VML namespaces.
        assert!(!out.contains("xmlns:v"));
    }

    #[test]
    fn gradient_declares_vml_and_reads_back_each_style() {
        for style in GradientStyle::ALL {
            let bg = PageBackground {
                color: 0xFFF2CC,
                gradient: Some(Gradient {
                    color2: 0x9DC3E6,
                    style,
                }),
            };
            let out = set_page_background(&doc(""), Some(&bg));
            assert!(
                out.contains(&format!(
                    "<w:document xmlns:w=\"W\" xmlns:v=\"{VML_NS}\" xmlns:o=\"{OFFICE_NS}\"><w:background"
                )),
                "{out}"
            );
            assert_eq!(page_background(&out), Some(bg), "{style:?}");
            // Declared once, however often it is applied.
            let again = set_page_background(&out, Some(&bg));
            assert_eq!(again, out);
        }
    }

    #[test]
    fn word_one_colour_gradient_reads_its_expression_as_white() {
        let word = doc(
            "<w:background w:color=\"FFF2CC\"><v:background id=\"_x0000_s1025\"><v:fill color2=\"fill darken(118)\" method=\"linear sigma\" focus=\"100%\" type=\"gradient\"/></v:background></w:background>",
        );
        let bg = page_background(&word).unwrap();
        assert_eq!(bg.color, 0xFFF2CC);
        assert_eq!(
            bg.gradient,
            Some(Gradient {
                color2: 0xFFFFFF,
                style: GradientStyle::Horizontal
            })
        );
    }

    #[test]
    fn only_the_prolog_background_is_seen() {
        // A `w:background` inside the body (malformed, but possible) is not
        // the page colour and is never removed.
        let body_bg =
            format!("{ROOT}<w:body><w:background w:color=\"FF0000\"/></w:body></w:document>");
        assert_eq!(page_background(&body_bg), None);
        assert_eq!(set_page_background(&body_bg, None), body_bg);
        // `auto` is no colour.
        assert_eq!(
            page_background(&doc("<w:background w:color=\"auto\"/>")),
            None
        );
    }
}
