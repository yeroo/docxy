//! Render Office MathML (OMML, the `m:` namespace) to a compact Unicode text
//! line, so equations show as readable text in a terminal that can't typeset
//! real math. Fractions become `a/b`, sub/superscripts use Unicode where they
//! exist (else `_`/`^`), n-ary operators keep their limits, delimiters keep
//! their brackets, and matrices render as a multi-row grid with growing
//! brackets (so a 2×2 stacks onto two lines).

use crate::mathbox::{MBox, flatten, hcat, matrix_grid};
use crate::xml::{Event, XmlParser};

/// Render an `<m:oMath>` / `<m:oMathPara>` element (given as raw XML) to text.
/// A matrix becomes a multi-line grid; the result may contain `\n`.
pub fn render_omath(xml: &str) -> String {
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if p.name() == "m:oMath" || p.name() == "m:oMathPara" => {
                let name = p.name().to_string();
                return flatten(&render_node(&mut p, &name));
            }
            Event::Eof => return String::new(),
            _ => {}
        }
    }
}

/// Read an `<m:m>` matrix into a grid of cell boxes and lay it out.
fn read_matrix(p: &mut XmlParser) -> MBox {
    let mut rows: Vec<Vec<MBox>> = Vec::new();
    loop {
        match p.next() {
            Event::Start if p.name() == "m:mr" => {
                let mut cells = Vec::new();
                loop {
                    match p.next() {
                        Event::Start if p.name() == "m:e" => cells.push(render_node(p, "m:e")),
                        Event::Start => p.skip_element(),
                        Event::End | Event::Eof => break,
                        Event::Text => {}
                    }
                }
                rows.push(cells);
            }
            Event::Start => p.skip_element(),
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    matrix_grid(&rows, '[', ']')
}

fn is_prop(name: &str) -> bool {
    name.ends_with("Pr") // rPr, fPr, dPr, naryPr, sSubPr, mPr, ctrlPr, …
}

/// A property element whose value lives in its `m:val` attribute.
fn is_char_prop(name: &str) -> bool {
    matches!(name, "m:chr" | "m:begChr" | "m:endChr" | "m:sepChr")
}

fn decode(raw: &str) -> String {
    let mut s = String::new();
    XmlParser::append_decoded(raw, &mut s);
    s
}

/// Read text up to the matching end tag, dropping OOXML's invisible math chars.
fn take_text(p: &mut XmlParser) -> String {
    let mut s = String::new();
    loop {
        match p.next() {
            Event::Text => XmlParser::append_decoded(p.text(), &mut s),
            Event::End | Event::Eof => break,
            Event::Start => p.skip_element(),
        }
    }
    s.chars()
        .filter(|c| !matches!(*c, '\u{2061}'..='\u{2064}'))
        .collect()
}

/// The (tag, rendered box) of each child element of the current element,
/// consuming through its end tag.
fn parts(p: &mut XmlParser) -> Vec<(String, MBox)> {
    let mut out = Vec::new();
    loop {
        match p.next() {
            Event::Start => {
                let n = p.name().to_string();
                if is_char_prop(&n) {
                    let v = decode(p.attr("m:val"));
                    p.skip_element();
                    out.push((n, MBox::line(v)));
                } else if is_prop(&n) {
                    // Properties are ignored, except the bracket/operator chars
                    // (m:chr / m:begChr / m:endChr) nested inside them.
                    for (t, v) in parts(p) {
                        if is_char_prop(&t) {
                            out.push((t, v));
                        }
                    }
                } else if n == "m:t" {
                    out.push((n, MBox::line(take_text(p))));
                } else {
                    let r = render_node(p, &n);
                    out.push((n, r));
                }
            }
            Event::Text => {}
            Event::End | Event::Eof => return out,
        }
    }
}

/// Render one element (its Start already consumed) into a math box.
fn render_node(p: &mut XmlParser, name: &str) -> MBox {
    if name == "m:m" {
        return read_matrix(p);
    }
    let ps = parts(p);
    // flattened-string view of the first child with a tag
    let s1 = |tag: &str| -> String {
        ps.iter()
            .find(|(t, _)| t == tag)
            .map(|(_, b)| b.flat())
            .unwrap_or_default()
    };
    let strs = |tag: &str| -> Vec<String> {
        ps.iter()
            .filter(|(t, _)| t == tag)
            .map(|(_, b)| b.flat())
            .collect()
    };

    match name {
        "m:f" => MBox::line(format!(
            "{}/{}",
            paren_if_op(&s1("m:num")),
            paren_if_op(&s1("m:den"))
        )),
        "m:d" => {
            let beg = first_nonempty(&strs("m:begChr")).unwrap_or_else(|| "(".to_string());
            let end = first_nonempty(&strs("m:endChr")).unwrap_or_else(|| ")".to_string());
            let elems: Vec<MBox> = ps
                .iter()
                .filter(|(t, _)| t == "m:e")
                .map(|(_, b)| b.clone())
                .collect();
            // A single matrix-like child keeps its own (tall) brackets; otherwise
            // wrap the inline content in the requested delimiter chars.
            if elems.len() == 1 && elems[0].lines.len() > 1 {
                elems.into_iter().next().unwrap()
            } else {
                let sep = first_nonempty(&strs("m:sepChr")).unwrap_or_else(|| "|".to_string());
                let inner = elems.iter().map(MBox::flat).collect::<Vec<_>>().join(&sep);
                MBox::line(format!("{beg}{inner}{end}"))
            }
        }
        "m:sSub" => MBox::line(format!("{}{}", s1("m:e"), to_script(&s1("m:sub"), false))),
        "m:sSup" => MBox::line(format!("{}{}", s1("m:e"), to_script(&s1("m:sup"), true))),
        "m:sSubSup" => MBox::line(format!(
            "{}{}{}",
            s1("m:e"),
            to_script(&s1("m:sub"), false),
            to_script(&s1("m:sup"), true)
        )),
        "m:nary" => {
            let op = first_nonempty(&strs("m:chr")).unwrap_or_else(|| "∫".to_string());
            let mut s = op;
            let sub = s1("m:sub");
            let sup = s1("m:sup");
            if !sub.is_empty() {
                s.push('_');
                s.push_str(&paren_if_op(&sub));
            }
            if !sup.is_empty() {
                s.push('^');
                s.push_str(&paren_if_op(&sup));
            }
            s.push(' ');
            s.push_str(&s1("m:e"));
            MBox::line(s)
        }
        "m:func" => MBox::line(format!("{}({})", s1("m:fName"), s1("m:e"))),
        "m:rad" => {
            let deg = s1("m:deg");
            if deg.is_empty() {
                MBox::line(format!("√({})", s1("m:e")))
            } else {
                MBox::line(format!("{deg}√({})", s1("m:e")))
            }
        }
        // generic containers (m:e, m:num, m:r, m:oMath, …): place children in a row
        _ => {
            let boxes: Vec<MBox> = ps
                .into_iter()
                .filter(|(t, _)| !is_prop(t))
                .map(|(_, b)| b)
                .collect();
            hcat(&boxes)
        }
    }
}

fn first_nonempty(v: &[String]) -> Option<String> {
    v.iter().find(|s| !s.is_empty()).cloned()
}

/// Parenthesize `s` if it contains an operator (so e.g. a fraction reads
/// `(x+1)/y`, but `12/5` stays bare).
fn paren_if_op(s: &str) -> String {
    if s.chars().count() > 1 && s.contains(['+', '-', '/', '=', ' ', '^', '·', '×']) {
        format!("({s})")
    } else {
        s.to_string()
    }
}

/// The superscript glyph for a character, if a well-supported one exists.
fn sup_char(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'n' => 'ⁿ',
        'i' => 'ⁱ',
        _ => return None,
    })
}

/// The subscript glyph for a character, if a well-supported one exists.
fn sub_char(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'x' => 'ₓ',
        _ => return None,
    })
}

/// Render a subscript/superscript: use Unicode glyphs when every character has
/// one, otherwise fall back to `_x` / `^x` (parenthesized if compound).
fn to_script(s: &str, sup: bool) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mapped: Option<String> = s
        .chars()
        .map(|c| if sup { sup_char(c) } else { sub_char(c) })
        .collect();
    match mapped {
        Some(m) => m,
        None => format!("{}{}", if sup { '^' } else { '_' }, paren_if_op(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(xml: &str) -> String {
        render_omath(xml)
    }

    #[test]
    fn fraction_and_runs() {
        let x = "<m:oMath><m:r><m:t>f</m:t></m:r><m:f><m:num><m:r><m:t>1</m:t></m:r></m:num>\
                 <m:den><m:r><m:t>2</m:t></m:r></m:den></m:f></m:oMath>";
        assert_eq!(r(x), "f1/2");
    }

    #[test]
    fn fraction_parenthesizes_compound_parts() {
        let x = "<m:oMath><m:f><m:num><m:r><m:t>x+1</m:t></m:r></m:num>\
                 <m:den><m:r><m:t>y</m:t></m:r></m:den></m:f></m:oMath>";
        assert_eq!(r(x), "(x+1)/y");
    }

    #[test]
    fn superscript_uses_unicode() {
        let x = "<m:oMath><m:sSup><m:e><m:r><m:t>x</m:t></m:r></m:e>\
                 <m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup></m:oMath>";
        assert_eq!(r(x), "x²");
    }

    #[test]
    fn delimiter_keeps_brackets() {
        let x = "<m:oMath><m:d><m:dPr><m:begChr m:val=\"[\"/><m:endChr m:val=\"]\"/></m:dPr>\
                 <m:e><m:r><m:t>x</m:t></m:r></m:e></m:d></m:oMath>";
        assert_eq!(r(x), "[x]");
    }

    #[test]
    fn delimiter_with_two_elements_uses_a_bar_separator() {
        let x = "<m:oMath><m:d><m:dPr><m:begChr m:val=\"⟨\"/><m:endChr m:val=\"⟩\"/></m:dPr>\
                 <m:e><m:r><m:t>12</m:t></m:r></m:e>\
                 <m:e><m:r><m:t>13</m:t></m:r></m:e></m:d></m:oMath>";
        assert_eq!(r(x), "⟨12|13⟩");
    }

    #[test]
    fn nary_keeps_operator_and_limits() {
        let x = "<m:oMath><m:nary><m:naryPr><m:chr m:val=\"∑\"/></m:naryPr>\
                 <m:sub><m:r><m:t>i=1</m:t></m:r></m:sub><m:sup><m:r><m:t>n</m:t></m:r></m:sup>\
                 <m:e><m:r><m:t>i</m:t></m:r></m:e></m:nary></m:oMath>";
        assert_eq!(r(x), "∑_(i=1)^n i");
    }

    #[test]
    fn real_equation_docx_renders_math_text() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../corpus/files/equation/equation.docx"
        );
        let Ok(data) = std::fs::read(path) else {
            return; // corpus not present
        };
        let pkg = crate::package::load_package(&data).expect("load");
        let opts = crate::render::RenderOptions {
            width: 90,
            ..Default::default()
        };
        let text: String = crate::render::render(&pkg.document, &opts)
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        // a fraction/superscript equation and a text matrix both appear
        assert!(
            text.contains("f(x)="),
            "missing equation:\n{}",
            &text[..text.len().min(300)]
        );
        assert!(text.contains("⎡ 1  0 ⎤"), "matrix not rendered as a grid");
    }

    #[test]
    fn matrix_renders_as_a_multi_row_grid() {
        let x = "<m:oMath><m:m><m:mr><m:e><m:r><m:t>a</m:t></m:r></m:e>\
                 <m:e><m:r><m:t>b</m:t></m:r></m:e></m:mr>\
                 <m:mr><m:e><m:r><m:t>c</m:t></m:r></m:e>\
                 <m:e><m:r><m:t>d</m:t></m:r></m:e></m:mr></m:m></m:oMath>";
        // two rows, columns aligned, with growing square brackets
        assert_eq!(r(x), "⎡ a  b ⎤\n⎣ c  d ⎦");
    }
}
