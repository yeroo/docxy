//! Render Office MathML (OMML, the `m:` namespace) to a compact Unicode text
//! line, so equations show as readable text in a terminal that can't typeset
//! real math. Fractions become `a/b`, sub/superscripts use Unicode where they
//! exist (else `_`/`^`), n-ary operators keep their limits, delimiters keep
//! their brackets, and matrices render as `[a, b; c, d]`.

use crate::xml::{Event, XmlParser};

/// Render an `<m:oMath>` / `<m:oMathPara>` element (given as raw XML) to text.
pub fn render_omath(xml: &str) -> String {
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if p.name() == "m:oMath" || p.name() == "m:oMathPara" => {
                let name = p.name().to_string();
                return render_node(&mut p, &name).trim().to_string();
            }
            Event::Eof => return String::new(),
            _ => {}
        }
    }
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

/// The (tag, rendered-text) of each child element of the current element,
/// consuming through its end tag.
fn parts(p: &mut XmlParser) -> Vec<(String, String)> {
    let mut out = Vec::new();
    loop {
        match p.next() {
            Event::Start => {
                let n = p.name().to_string();
                if is_char_prop(&n) {
                    let v = decode(p.attr("m:val"));
                    p.skip_element();
                    out.push((n, v));
                } else if is_prop(&n) {
                    // Properties are ignored, except the bracket/operator chars
                    // (m:chr / m:begChr / m:endChr) nested inside them.
                    for (t, v) in parts(p) {
                        if is_char_prop(&t) {
                            out.push((t, v));
                        }
                    }
                } else if n == "m:t" {
                    out.push((n, take_text(p)));
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

/// Render one element (its Start already consumed) by combining its children.
fn render_node(p: &mut XmlParser, name: &str) -> String {
    let ps = parts(p);
    let one = |tag: &str| -> String {
        ps.iter()
            .find(|(t, _)| t == tag)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let all = |tag: &str| -> Vec<String> {
        ps.iter()
            .filter(|(t, _)| t == tag)
            .map(|(_, v)| v.clone())
            .collect()
    };
    let concat = || -> String {
        ps.iter()
            .filter(|(t, _)| !is_prop(t))
            .map(|(_, v)| v.as_str())
            .collect()
    };

    match name {
        "m:f" => format!(
            "{}/{}",
            paren_if_op(&one("m:num")),
            paren_if_op(&one("m:den"))
        ),
        "m:d" => {
            let beg = first_nonempty(&all("m:begChr")).unwrap_or_else(|| "(".to_string());
            let end = first_nonempty(&all("m:endChr")).unwrap_or_else(|| ")".to_string());
            // OMML's default separator is a vertical bar, not a comma.
            let sep = first_nonempty(&all("m:sepChr")).unwrap_or_else(|| "|".to_string());
            let inner = all("m:e").join(&sep);
            format!("{beg}{inner}{end}")
        }
        "m:sSub" => format!("{}{}", one("m:e"), to_script(&one("m:sub"), false)),
        "m:sSup" => format!("{}{}", one("m:e"), to_script(&one("m:sup"), true)),
        "m:sSubSup" => format!(
            "{}{}{}",
            one("m:e"),
            to_script(&one("m:sub"), false),
            to_script(&one("m:sup"), true)
        ),
        "m:nary" => {
            let op = first_nonempty(&all("m:chr")).unwrap_or_else(|| "∫".to_string());
            let mut s = op;
            let sub = one("m:sub");
            let sup = one("m:sup");
            if !sub.is_empty() {
                s.push('_');
                s.push_str(&paren_if_op(&sub));
            }
            if !sup.is_empty() {
                s.push('^');
                s.push_str(&paren_if_op(&sup));
            }
            s.push(' ');
            s.push_str(&one("m:e"));
            s
        }
        "m:func" => format!("{}({})", one("m:fName"), one("m:e")),
        "m:rad" => {
            let deg = one("m:deg");
            if deg.is_empty() {
                format!("√({})", one("m:e"))
            } else {
                format!("{deg}√({})", one("m:e"))
            }
        }
        "m:m" => format!("[{}]", all("m:mr").join("; ")),
        "m:mr" => all("m:e").join(", "),
        // generic containers: m:e, m:num, m:den, m:sub, m:sup, m:fName, m:r,
        // m:oMath, m:oMathPara …
        _ => concat(),
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
        assert!(text.contains("[1, 0; 0, 1]"), "matrix not text-rendered");
    }

    #[test]
    fn matrix_renders_as_text_grid() {
        let x = "<m:oMath><m:m><m:mr><m:e><m:r><m:t>a</m:t></m:r></m:e>\
                 <m:e><m:r><m:t>b</m:t></m:r></m:e></m:mr>\
                 <m:mr><m:e><m:r><m:t>c</m:t></m:r></m:e>\
                 <m:e><m:r><m:t>d</m:t></m:r></m:e></m:mr></m:m></m:oMath>";
        assert_eq!(r(x), "[a, b; c, d]");
    }
}
