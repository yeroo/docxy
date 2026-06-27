//! LaTeX ⇄ Office MathML (OMML) for a common scientific subset.
//!
//! This bridges Markdown math (`$…$` / `$$…$$`, LaTeX inside) and Word's native
//! equations (`<m:oMath>` / `<m:oMathPara>`), so a formula authored in Markdown
//! becomes a real Word equation on save, and a Word equation becomes LaTeX on
//! Markdown export.
//!
//! Supported in both directions: identifiers/operators, Greek letters and common
//! symbols, `^`/`_` scripts, `\frac`, `\sqrt` (with optional index), the n-ary
//! operators `\sum`/`\int`/`\prod` (and friends) with limits, `\left…\right`
//! delimiters, and named functions (`\sin`, `\log`, …). Anything outside the
//! subset degrades gracefully: unknown commands pass through as literal text, and
//! OMML constructs we don't model are flattened to their inner text.

// ---------------------------------------------------------------------------
// Symbol tables (shared by both directions).
// ---------------------------------------------------------------------------

/// LaTeX command (without the leading backslash) → the Unicode glyph used in
/// OMML text. The reverse direction prefers the *first* command that maps to a
/// glyph, so list canonical spellings first.
const SYMBOLS: &[(&str, &str)] = &[
    // Lowercase Greek.
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ε"),
    ("varepsilon", "ε"),
    ("zeta", "ζ"),
    ("eta", "η"),
    ("theta", "θ"),
    ("vartheta", "ϑ"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("nu", "ν"),
    ("xi", "ξ"),
    ("pi", "π"),
    ("rho", "ρ"),
    ("sigma", "σ"),
    ("tau", "τ"),
    ("upsilon", "υ"),
    ("phi", "φ"),
    ("varphi", "ϕ"),
    ("chi", "χ"),
    ("psi", "ψ"),
    ("omega", "ω"),
    // Uppercase Greek.
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Theta", "Θ"),
    ("Lambda", "Λ"),
    ("Xi", "Ξ"),
    ("Pi", "Π"),
    ("Sigma", "Σ"),
    ("Upsilon", "Υ"),
    ("Phi", "Φ"),
    ("Psi", "Ψ"),
    ("Omega", "Ω"),
    // Binary / relational operators.
    ("times", "×"),
    ("div", "÷"),
    ("cdot", "⋅"),
    ("pm", "±"),
    ("mp", "∓"),
    ("ast", "∗"),
    ("star", "⋆"),
    ("circ", "∘"),
    ("bullet", "∙"),
    ("leq", "≤"),
    ("le", "≤"),
    ("geq", "≥"),
    ("ge", "≥"),
    ("neq", "≠"),
    ("ne", "≠"),
    ("equiv", "≡"),
    ("approx", "≈"),
    ("cong", "≅"),
    ("sim", "∼"),
    ("propto", "∝"),
    ("ll", "≪"),
    ("gg", "≫"),
    // Arrows.
    ("to", "→"),
    ("rightarrow", "→"),
    ("leftarrow", "←"),
    ("leftrightarrow", "↔"),
    ("Rightarrow", "⇒"),
    ("Leftarrow", "⇐"),
    ("Leftrightarrow", "⇔"),
    ("mapsto", "↦"),
    // Set / logic.
    ("in", "∈"),
    ("notin", "∉"),
    ("ni", "∋"),
    ("subset", "⊂"),
    ("subseteq", "⊆"),
    ("supset", "⊃"),
    ("supseteq", "⊇"),
    ("cup", "∪"),
    ("cap", "∩"),
    ("setminus", "∖"),
    ("emptyset", "∅"),
    ("forall", "∀"),
    ("exists", "∃"),
    ("nabla", "∇"),
    ("partial", "∂"),
    ("land", "∧"),
    ("lor", "∨"),
    ("neg", "¬"),
    // Misc.
    ("infty", "∞"),
    ("aleph", "ℵ"),
    ("hbar", "ℏ"),
    ("ell", "ℓ"),
    ("Re", "ℜ"),
    ("Im", "ℑ"),
    ("angle", "∠"),
    ("perp", "⊥"),
    ("parallel", "∥"),
    ("degree", "°"),
    ("prime", "′"),
    ("cdots", "⋯"),
    ("ldots", "…"),
    ("dots", "…"),
    ("vdots", "⋮"),
    ("ddots", "⋱"),
];

/// N-ary operator command → its OMML `m:chr` glyph.
const NARY: &[(&str, &str)] = &[
    ("sum", "∑"),
    ("prod", "∏"),
    ("coprod", "∐"),
    ("int", "∫"),
    ("iint", "∬"),
    ("iiint", "∭"),
    ("oint", "∮"),
    ("bigcup", "⋃"),
    ("bigcap", "⋂"),
    ("bigvee", "⋁"),
    ("bigwedge", "⋀"),
    ("bigoplus", "⨁"),
    ("bigotimes", "⨂"),
];

/// Named functions that render as upright identifiers (`\sin` → `sin`).
const FUNCTIONS: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "sinh", "cosh", "tanh", "arcsin", "arccos", "arctan",
    "log", "ln", "lg", "exp", "lim", "limsup", "liminf", "max", "min", "sup", "inf", "det", "dim",
    "ker", "deg", "gcd", "arg", "hom",
];

fn symbol_glyph(cmd: &str) -> Option<&'static str> {
    SYMBOLS.iter().find(|(c, _)| *c == cmd).map(|(_, g)| *g)
}
fn nary_glyph(cmd: &str) -> Option<&'static str> {
    NARY.iter().find(|(c, _)| *c == cmd).map(|(_, g)| *g)
}

// ===========================================================================
// LaTeX → OMML
// ===========================================================================

/// A parsed LaTeX node (a small math AST). Sequences are `Vec<Node>`.
#[derive(Debug, Clone)]
enum Node {
    /// Literal text destined for an `<m:t>` (already glyph-mapped).
    Text(String),
    /// A transparent brace group `{…}` — emitted as a plain row of children.
    Group(Vec<Node>),
    /// `base^{sup}` / `base_{sub}` / `base_{sub}^{sup}`.
    Script {
        base: Vec<Node>,
        sub: Option<Vec<Node>>,
        sup: Option<Vec<Node>>,
    },
    /// `\frac{num}{den}`.
    Frac(Vec<Node>, Vec<Node>),
    /// `\sqrt[deg]{radicand}` (deg optional).
    Sqrt(Option<Vec<Node>>, Vec<Node>),
    /// An n-ary operator with limits and an operand body.
    Nary {
        chr: String,
        sub: Option<Vec<Node>>,
        sup: Option<Vec<Node>>,
        body: Vec<Node>,
    },
    /// `\left( … \right)` or a bare `( … )` delimiter.
    Delim(char, char, Vec<Node>),
    /// A named function applied to the rest of the sequence (`\sin x`).
    Func(String, Vec<Node>),
}

/// Convert a LaTeX formula to OMML. With `display`, wraps in `<m:oMathPara>`
/// (block / `$$…$$`); otherwise a plain inline `<m:oMath>`.
pub fn latex_to_omml(latex: &str, display: bool) -> String {
    let toks = lex(latex);
    let mut pos = 0;
    let nodes = parse_seq(&toks, &mut pos, None);
    let inner = emit_seq(&nodes);
    if display {
        format!("<m:oMathPara><m:oMath>{inner}</m:oMath></m:oMathPara>")
    } else {
        format!("<m:oMath>{inner}</m:oMath>")
    }
}

/// A LaTeX token.
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// `\command` (letters) or a single-character control symbol (`\,`, `\{`).
    Cmd(String),
    Char(char),
    OpenBrace,
    CloseBrace,
    Caret,
    Underscore,
}

fn lex(s: &str) -> Vec<Tok> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                i += 1;
                if i >= chars.len() {
                    break;
                }
                if chars[i].is_ascii_alphabetic() {
                    let mut name = String::new();
                    while i < chars.len() && chars[i].is_ascii_alphabetic() {
                        name.push(chars[i]);
                        i += 1;
                    }
                    out.push(Tok::Cmd(name));
                } else {
                    // Control symbol: `\{`, `\}`, `\,`, `\ `, `\\`, …
                    out.push(Tok::Cmd(chars[i].to_string()));
                    i += 1;
                }
            }
            '{' => {
                out.push(Tok::OpenBrace);
                i += 1;
            }
            '}' => {
                out.push(Tok::CloseBrace);
                i += 1;
            }
            '^' => {
                out.push(Tok::Caret);
                i += 1;
            }
            '_' => {
                out.push(Tok::Underscore);
                i += 1;
            }
            c if c.is_whitespace() => {
                i += 1;
            } // collapse intertoken space
            c => {
                out.push(Tok::Char(c));
                i += 1;
            }
        }
    }
    out
}

/// Parse a sequence until `stop` (a closing brace, when nested) or end.
fn parse_seq(toks: &[Tok], pos: &mut usize, stop: Option<&Tok>) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::new();
    while *pos < toks.len() {
        if let Some(s) = stop {
            if &toks[*pos] == s {
                break;
            }
        }
        match &toks[*pos] {
            Tok::CloseBrace => break,
            Tok::Caret | Tok::Underscore => {
                // A script with no preceding atom: attach to an empty base.
                let base = out.pop().map(|n| vec![n]).unwrap_or_default();
                out.push(parse_scripts(toks, pos, base));
            }
            _ => {
                let atom = parse_atom(toks, pos);
                // An n-ary operator or function swallows the remainder as its body.
                match atom {
                    Atom::Nary { chr, sub, sup } => {
                        let (sub, sup) = if sub.is_some() || sup.is_some() {
                            (sub, sup)
                        } else {
                            parse_limits(toks, pos)
                        };
                        let body = parse_seq(toks, pos, stop);
                        out.push(Node::Nary {
                            chr,
                            sub,
                            sup,
                            body,
                        });
                        break;
                    }
                    Atom::Func(name) => {
                        let body = parse_seq(toks, pos, stop);
                        out.push(Node::Func(name, body));
                        break;
                    }
                    Atom::Node(n) => {
                        // Trailing scripts bind to this atom.
                        if matches!(toks.get(*pos), Some(Tok::Caret) | Some(Tok::Underscore)) {
                            out.push(parse_scripts(toks, pos, vec![n]));
                        } else {
                            out.push(n);
                        }
                    }
                }
            }
        }
    }
    out
}

/// A single parsed unit, before scripts are attached.
enum Atom {
    Node(Node),
    Nary {
        chr: String,
        sub: Option<Vec<Node>>,
        sup: Option<Vec<Node>>,
    },
    Func(String),
}

fn parse_atom(toks: &[Tok], pos: &mut usize) -> Atom {
    match toks[*pos].clone() {
        Tok::OpenBrace => {
            *pos += 1;
            let inner = parse_seq(toks, pos, None);
            expect_close(toks, pos);
            // A brace group is transparent: re-emit its nodes in a row.
            Atom::Node(group(inner))
        }
        Tok::Char('(') | Tok::Char('[') | Tok::Char('|') => {
            // Bare delimiter run: pair with its partner so brackets grow.
            let open = if let Tok::Char(c) = toks[*pos] {
                c
            } else {
                '('
            };
            *pos += 1;
            let close = match open {
                '(' => ')',
                '[' => ']',
                _ => '|',
            };
            let inner = parse_until_char(toks, pos, close);
            Atom::Node(Node::Delim(open, close, inner))
        }
        Tok::Char(c) => {
            *pos += 1;
            Atom::Node(Node::Text(c.to_string()))
        }
        Tok::Cmd(name) => {
            *pos += 1;
            parse_command(&name, toks, pos)
        }
        // Stray brace/script handled by caller; treat as empty.
        _ => {
            *pos += 1;
            Atom::Node(Node::Text(String::new()))
        }
    }
}

fn parse_command(name: &str, toks: &[Tok], pos: &mut usize) -> Atom {
    match name {
        "frac" | "dfrac" | "tfrac" => {
            let num = parse_arg(toks, pos);
            let den = parse_arg(toks, pos);
            Atom::Node(Node::Frac(num, den))
        }
        "sqrt" => {
            let deg = parse_optional_arg(toks, pos);
            let rad = parse_arg(toks, pos);
            Atom::Node(Node::Sqrt(deg, rad))
        }
        "left" => {
            let open = take_delim_char(toks, pos, true);
            let inner = parse_until_right(toks, pos);
            let close = take_delim_char(toks, pos, false);
            Atom::Node(Node::Delim(open, close, inner))
        }
        // Spacing commands → a thin space (or nothing for negative space).
        "," | ":" | ";" | " " | "quad" | "qquad" | "!" => {
            Atom::Node(Node::Text(if name == "!" { "" } else { " " }.to_string()))
        }
        "{" | "}" | "%" | "$" | "#" | "&" | "_" => Atom::Node(Node::Text(name.to_string())),
        "\\" => Atom::Node(Node::Text(String::new())), // line break: ignore inline
        _ => {
            if let Some(chr) = nary_glyph(name) {
                Atom::Nary {
                    chr: chr.to_string(),
                    sub: None,
                    sup: None,
                }
            } else if FUNCTIONS.contains(&name) {
                Atom::Func(name.to_string())
            } else if let Some(g) = symbol_glyph(name) {
                Atom::Node(Node::Text(g.to_string()))
            } else {
                // Unknown command: pass the bare name through as text.
                Atom::Node(Node::Text(name.to_string()))
            }
        }
    }
}

/// Parse `_{…}`/`^{…}` (in any order) following `base`.
fn parse_scripts(toks: &[Tok], pos: &mut usize, base: Vec<Node>) -> Node {
    let (sub, sup) = parse_limits(toks, pos);
    Node::Script { base, sub, sup }
}

/// Parse the optional sub/superscript limits after a base or n-ary operator.
fn parse_limits(toks: &[Tok], pos: &mut usize) -> (Option<Vec<Node>>, Option<Vec<Node>>) {
    let mut sub = None;
    let mut sup = None;
    loop {
        match toks.get(*pos) {
            Some(Tok::Underscore) => {
                *pos += 1;
                sub = Some(parse_arg(toks, pos));
            }
            Some(Tok::Caret) => {
                *pos += 1;
                sup = Some(parse_arg(toks, pos));
            }
            _ => break,
        }
    }
    (sub, sup)
}

/// Parse a single `{…}` argument, or one atom if unbraced (`x^2`).
fn parse_arg(toks: &[Tok], pos: &mut usize) -> Vec<Node> {
    match toks.get(*pos) {
        Some(Tok::OpenBrace) => {
            *pos += 1;
            let inner = parse_seq(toks, pos, None);
            expect_close(toks, pos);
            inner
        }
        Some(_) => match parse_atom(toks, pos) {
            Atom::Node(n) => vec![n],
            Atom::Nary { chr, .. } => vec![Node::Text(chr)],
            Atom::Func(name) => vec![Node::Text(name)],
        },
        None => Vec::new(),
    }
}

/// Parse a `[…]` optional argument (the root index of `\sqrt[n]{…}`).
fn parse_optional_arg(toks: &[Tok], pos: &mut usize) -> Option<Vec<Node>> {
    if toks.get(*pos) == Some(&Tok::Char('[')) {
        *pos += 1;
        let inner = parse_until_char(toks, pos, ']');
        Some(inner)
    } else {
        None
    }
}

fn parse_until_char(toks: &[Tok], pos: &mut usize, close: char) -> Vec<Node> {
    let mut out = Vec::new();
    while *pos < toks.len() {
        if toks[*pos] == Tok::Char(close) {
            *pos += 1;
            break;
        }
        if toks[*pos] == Tok::CloseBrace {
            break;
        }
        match parse_atom(toks, pos) {
            Atom::Node(n) => {
                if matches!(toks.get(*pos), Some(Tok::Caret) | Some(Tok::Underscore)) {
                    out.push(parse_scripts(toks, pos, vec![n]));
                } else {
                    out.push(n);
                }
            }
            Atom::Nary { chr, .. } => out.push(Node::Text(chr)),
            Atom::Func(name) => out.push(Node::Text(name)),
        }
    }
    out
}

/// Parse up to a matching `\right` (consumed by the caller via `take_delim_char`).
fn parse_until_right(toks: &[Tok], pos: &mut usize) -> Vec<Node> {
    let mut out = Vec::new();
    while *pos < toks.len() {
        if toks[*pos] == Tok::Cmd("right".to_string()) {
            *pos += 1;
            break;
        }
        match parse_atom(toks, pos) {
            Atom::Node(n) => {
                if matches!(toks.get(*pos), Some(Tok::Caret) | Some(Tok::Underscore)) {
                    out.push(parse_scripts(toks, pos, vec![n]));
                } else {
                    out.push(n);
                }
            }
            Atom::Nary { chr, sub, sup } => {
                let (sub, sup) = if sub.is_some() || sup.is_some() {
                    (sub, sup)
                } else {
                    parse_limits(toks, pos)
                };
                let body = parse_until_right(toks, pos);
                out.push(Node::Nary {
                    chr,
                    sub,
                    sup,
                    body,
                });
                break;
            }
            Atom::Func(name) => {
                let body = parse_until_right(toks, pos);
                out.push(Node::Func(name, body));
                break;
            }
        }
    }
    out
}

/// Read the delimiter character following `\left` / `\right`. `.` means none.
fn take_delim_char(toks: &[Tok], pos: &mut usize, open: bool) -> char {
    let fallback = if open { '(' } else { ')' };
    match toks.get(*pos) {
        Some(Tok::Char('.')) => {
            *pos += 1;
            '\u{0}'
        } // null = no bracket
        Some(Tok::Char(c)) => {
            let c = *c;
            *pos += 1;
            c
        }
        Some(Tok::Cmd(name)) => {
            // \{ , \langle , \| , etc.
            let c = match name.as_str() {
                "{" => '{',
                "}" => '}',
                "langle" => '⟨',
                "rangle" => '⟩',
                "|" | "Vert" | "vert" => '|',
                "lfloor" => '⌊',
                "rfloor" => '⌋',
                "lceil" => '⌈',
                "rceil" => '⌉',
                _ => fallback,
            };
            *pos += 1;
            c
        }
        _ => fallback,
    }
}

fn expect_close(toks: &[Tok], pos: &mut usize) {
    if toks.get(*pos) == Some(&Tok::CloseBrace) {
        *pos += 1;
    }
}

/// Wrap a parsed sequence as a single transparent node (a brace group).
fn group(nodes: Vec<Node>) -> Node {
    if nodes.len() == 1 {
        nodes.into_iter().next().unwrap()
    } else {
        Node::Group(nodes)
    }
}

// ---- OMML emission --------------------------------------------------------

fn emit_seq(nodes: &[Node]) -> String {
    let mut s = String::new();
    // Merge consecutive Text nodes into one run for tidy OMML.
    let mut buf = String::new();
    let flush = |buf: &mut String, s: &mut String| {
        if !buf.is_empty() {
            s.push_str(&run(buf));
            buf.clear();
        }
    };
    for n in nodes {
        match n {
            Node::Text(t) => buf.push_str(t),
            other => {
                flush(&mut buf, &mut s);
                s.push_str(&emit_node(other));
            }
        }
    }
    flush(&mut buf, &mut s);
    s
}

fn emit_node(n: &Node) -> String {
    match n {
        Node::Text(t) => run(t),
        Node::Group(nodes) => emit_seq(nodes),
        Node::Script { base, sub, sup } => {
            let b = format!("<m:e>{}</m:e>", emit_seq(base));
            match (sub, sup) {
                (Some(sub), Some(sup)) => format!(
                    "<m:sSubSup>{b}<m:sub>{}</m:sub><m:sup>{}</m:sup></m:sSubSup>",
                    emit_seq(sub),
                    emit_seq(sup)
                ),
                (Some(sub), None) => {
                    format!("<m:sSub>{b}<m:sub>{}</m:sub></m:sSub>", emit_seq(sub))
                }
                (None, Some(sup)) => {
                    format!("<m:sSup>{b}<m:sup>{}</m:sup></m:sSup>", emit_seq(sup))
                }
                (None, None) => emit_seq(base),
            }
        }
        Node::Frac(num, den) => format!(
            "<m:f><m:num>{}</m:num><m:den>{}</m:den></m:f>",
            emit_seq(num),
            emit_seq(den)
        ),
        Node::Sqrt(deg, rad) => match deg {
            None => format!(
                "<m:rad><m:radPr><m:degHide m:val=\"1\"/></m:radPr><m:deg/><m:e>{}</m:e></m:rad>",
                emit_seq(rad)
            ),
            Some(d) => format!(
                "<m:rad><m:deg>{}</m:deg><m:e>{}</m:e></m:rad>",
                emit_seq(d),
                emit_seq(rad)
            ),
        },
        Node::Nary {
            chr,
            sub,
            sup,
            body,
        } => {
            let sub = sub.as_ref().map(|s| emit_seq(s)).unwrap_or_default();
            let sup = sup.as_ref().map(|s| emit_seq(s)).unwrap_or_default();
            format!(
                "<m:nary><m:naryPr><m:chr m:val=\"{}\"/><m:limLoc m:val=\"subSup\"/></m:naryPr>\
                 <m:sub>{sub}</m:sub><m:sup>{sup}</m:sup><m:e>{}</m:e></m:nary>",
                esc_attr(chr),
                emit_seq(body)
            )
        }
        Node::Delim(open, close, inner) => {
            let beg = if *open == '\u{0}' {
                String::new()
            } else {
                open.to_string()
            };
            let end = if *close == '\u{0}' {
                String::new()
            } else {
                close.to_string()
            };
            format!(
                "<m:d><m:dPr><m:begChr m:val=\"{}\"/><m:endChr m:val=\"{}\"/></m:dPr>\
                 <m:e>{}</m:e></m:d>",
                esc_attr(&beg),
                esc_attr(&end),
                emit_seq(inner)
            )
        }
        Node::Func(name, body) => format!(
            "<m:func><m:fName>{}</m:fName><m:e>{}</m:e></m:func>",
            run(name),
            emit_seq(body)
        ),
    }
}

/// An OMML text run, or empty string for empty text.
fn run(text: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!("<m:r><m:t>{}</m:t></m:r>", esc_text(text))
    }
}

fn esc_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn esc_attr(s: &str) -> String {
    esc_text(s).replace('"', "&quot;")
}

// ===========================================================================
// OMML → LaTeX
// ===========================================================================

use crate::xml::{Event, XmlParser};

/// Reverse a Unicode glyph to its canonical LaTeX command (with backslash).
fn glyph_to_latex(c: char) -> Option<String> {
    let s = c.to_string();
    SYMBOLS
        .iter()
        .find(|(_, g)| *g == s)
        .map(|(cmd, _)| format!("\\{cmd}"))
        .or_else(|| {
            NARY.iter()
                .find(|(_, g)| *g == s)
                .map(|(cmd, _)| format!("\\{cmd}"))
        })
}

/// Convert an `<m:oMath>` / `<m:oMathPara>` element (raw XML) to LaTeX (no `$`).
pub fn omml_to_latex(xml: &str) -> String {
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if p.name() == "m:oMath" || p.name() == "m:oMathPara" => {
                let name = p.name().to_string();
                return seq_latex(&mut p, &name).trim().to_string();
            }
            Event::Eof => return String::new(),
            _ => {}
        }
    }
}

fn is_prop(name: &str) -> bool {
    name.ends_with("Pr")
}

/// Render the children of the current element (its Start already consumed) as a
/// LaTeX sequence, stopping at the matching end tag.
fn seq_latex(p: &mut XmlParser, _name: &str) -> String {
    let mut out = String::new();
    loop {
        match p.next() {
            Event::Start => {
                let n = p.name().to_string();
                if is_prop(&n) {
                    p.skip_element();
                } else {
                    out.push_str(&node_latex(p, &n));
                }
            }
            Event::Text => {}
            Event::End | Event::Eof => break,
        }
    }
    out
}

/// Collect the (tag, latex) of each non-property child of the current element.
fn children_latex(p: &mut XmlParser) -> Vec<(String, String)> {
    let mut out = Vec::new();
    loop {
        match p.next() {
            Event::Start => {
                let n = p.name().to_string();
                if n == "m:chr" || n == "m:begChr" || n == "m:endChr" {
                    let v = decode_attr(p.attr("m:val"));
                    p.skip_element();
                    out.push((n, v));
                } else if is_prop(&n) {
                    // Keep nested char props (e.g. m:chr inside m:naryPr).
                    for (t, v) in children_latex(p) {
                        if t == "m:chr" || t == "m:begChr" || t == "m:endChr" {
                            out.push((t, v));
                        }
                    }
                } else if n == "m:t" {
                    out.push((n, text_latex(p)));
                } else {
                    let v = node_latex(p, &n);
                    out.push((n, v));
                }
            }
            Event::Text => {}
            Event::End | Event::Eof => return out,
        }
    }
}

fn node_latex(p: &mut XmlParser, name: &str) -> String {
    let kids = children_latex(p);
    let first = |tag: &str| -> String {
        kids.iter()
            .find(|(t, _)| t == tag)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let firsts = |tag: &str| -> Vec<String> {
        kids.iter()
            .filter(|(t, _)| t == tag)
            .map(|(_, v)| v.clone())
            .collect()
    };
    match name {
        "m:f" => format!("\\frac{{{}}}{{{}}}", first("m:num"), first("m:den")),
        "m:rad" => {
            let deg = first("m:deg");
            if deg.is_empty() {
                format!("\\sqrt{{{}}}", first("m:e"))
            } else {
                format!("\\sqrt[{}]{{{}}}", deg, first("m:e"))
            }
        }
        "m:sSup" => format!("{}^{{{}}}", brace_base(&first("m:e")), first("m:sup")),
        "m:sSub" => format!("{}_{{{}}}", brace_base(&first("m:e")), first("m:sub")),
        "m:sSubSup" => format!(
            "{}_{{{}}}^{{{}}}",
            brace_base(&first("m:e")),
            first("m:sub"),
            first("m:sup")
        ),
        "m:nary" => {
            let chr = firsts("m:chr").into_iter().find(|s| !s.is_empty());
            let op = chr
                .as_deref()
                .and_then(|c| c.chars().next())
                .and_then(glyph_to_latex)
                .unwrap_or_else(|| "\\int".to_string());
            let mut s = op;
            let sub = first("m:sub");
            let sup = first("m:sup");
            if !sub.is_empty() {
                s.push_str(&format!("_{{{sub}}}"));
            }
            if !sup.is_empty() {
                s.push_str(&format!("^{{{sup}}}"));
            }
            s.push(' ');
            s.push_str(&first("m:e"));
            s
        }
        "m:d" => {
            let beg = firsts("m:begChr").into_iter().find(|s| !s.is_empty());
            let end = firsts("m:endChr").into_iter().find(|s| !s.is_empty());
            let beg = beg.as_deref().unwrap_or("(");
            let end = end.as_deref().unwrap_or(")");
            let inner: String = kids
                .iter()
                .filter(|(t, _)| t == "m:e")
                .map(|(_, v)| v.clone())
                .collect::<Vec<_>>()
                .join(",");
            format!("{beg}{inner}{end}")
        }
        "m:func" => format!("{} {}", first("m:fName"), first("m:e")),
        // Generic container: concatenate non-property children.
        _ => kids
            .iter()
            .filter(|(t, _)| !is_prop(t))
            .map(|(_, v)| v.clone())
            .collect(),
    }
}

/// Read an `<m:t>` run and map each glyph back to LaTeX.
fn text_latex(p: &mut XmlParser) -> String {
    let mut raw = String::new();
    loop {
        match p.next() {
            Event::Text => XmlParser::append_decoded(p.text(), &mut raw),
            Event::Start => p.skip_element(),
            Event::End | Event::Eof => break,
        }
    }
    let mut out = String::new();
    for c in raw.chars() {
        if matches!(c, '\u{2061}'..='\u{2064}') {
            continue; // invisible OOXML math operators
        }
        match glyph_to_latex(c) {
            Some(cmd) => {
                out.push_str(&cmd);
                out.push(' '); // separate a word command from following text
            }
            None => out.push(c),
        }
    }
    out
}

fn decode_attr(raw: &str) -> String {
    let mut s = String::new();
    XmlParser::append_decoded(raw, &mut s);
    s
}

/// Wrap a script base in braces unless it's a single token, so `x^2` stays bare
/// but `(a+b)^2` keeps its grouping.
fn brace_base(base: &str) -> String {
    let n = base.chars().count();
    if n <= 1 || (base.starts_with('\\') && !base[1..].contains(char::is_whitespace)) {
        base.to_string()
    } else {
        format!("{{{base}}}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LaTeX → OMML (inline), as a string.
    fn omml(latex: &str) -> String {
        latex_to_omml(latex, false)
    }

    #[test]
    fn superscript_to_omml() {
        let x = omml("x^2");
        assert!(
            x.contains("<m:sSup><m:e><m:r><m:t>x</m:t></m:r></m:e><m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup>"),
            "{x}"
        );
    }

    #[test]
    fn fraction_to_omml() {
        let x = omml("\\frac{a}{b}");
        assert!(
            x.contains("<m:f><m:num><m:r><m:t>a</m:t></m:r></m:num>"),
            "{x}"
        );
        assert!(
            x.contains("<m:den><m:r><m:t>b</m:t></m:r></m:den></m:f>"),
            "{x}"
        );
    }

    #[test]
    fn greek_and_operators_map_to_glyphs() {
        let x = omml("\\alpha \\times \\beta");
        assert!(x.contains("α"), "{x}");
        assert!(x.contains("×"), "{x}");
        assert!(x.contains("β"), "{x}");
    }

    #[test]
    fn sqrt_with_and_without_index() {
        assert!(omml("\\sqrt{x}").contains("<m:degHide m:val=\"1\"/>"));
        let n = omml("\\sqrt[3]{x}");
        assert!(n.contains("<m:deg><m:r><m:t>3</m:t></m:r></m:deg>"), "{n}");
    }

    #[test]
    fn nary_sum_with_limits() {
        let x = omml("\\sum_{i=1}^{n} i");
        assert!(x.contains("<m:chr m:val=\"∑\"/>"), "{x}");
        assert!(
            x.contains("<m:sub><m:r><m:t>i=1</m:t></m:r></m:sub>"),
            "{x}"
        );
        assert!(x.contains("<m:sup><m:r><m:t>n</m:t></m:r></m:sup>"), "{x}");
    }

    #[test]
    fn display_wraps_in_omathpara() {
        let x = latex_to_omml("x", true);
        assert!(x.starts_with("<m:oMathPara><m:oMath>"), "{x}");
        assert!(x.ends_with("</m:oMath></m:oMathPara>"), "{x}");
    }

    // ---- reverse: OMML → LaTeX -------------------------------------------

    #[test]
    fn omml_fraction_to_latex() {
        let xml = "<m:oMath><m:f><m:num><m:r><m:t>a</m:t></m:r></m:num>\
                   <m:den><m:r><m:t>b</m:t></m:r></m:den></m:f></m:oMath>";
        assert_eq!(omml_to_latex(xml), "\\frac{a}{b}");
    }

    #[test]
    fn omml_superscript_to_latex() {
        let xml = "<m:oMath><m:sSup><m:e><m:r><m:t>x</m:t></m:r></m:e>\
                   <m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup></m:oMath>";
        assert_eq!(omml_to_latex(xml), "x^{2}");
    }

    #[test]
    fn omml_glyph_back_to_command() {
        let xml = "<m:oMath><m:r><m:t>α×β</m:t></m:r></m:oMath>";
        assert_eq!(omml_to_latex(xml), "\\alpha \\times \\beta".trim());
    }

    // ---- round-trips through OMML ----------------------------------------

    /// LaTeX → OMML → LaTeX should be stable for the supported subset.
    fn roundtrip(latex: &str) -> String {
        omml_to_latex(&latex_to_omml(latex, false))
    }

    #[test]
    fn roundtrips_are_stable() {
        assert_eq!(roundtrip("x^{2}"), "x^{2}");
        assert_eq!(roundtrip("\\frac{a}{b}"), "\\frac{a}{b}");
        assert_eq!(roundtrip("\\sqrt{x+1}"), "\\sqrt{x+1}");
        assert_eq!(roundtrip("\\sum_{i=1}^{n} i"), "\\sum_{i=1}^{n} i");
        assert_eq!(roundtrip("E=mc^{2}"), "E=mc^{2}");
        assert_eq!(roundtrip("\\alpha +\\beta"), "\\alpha +\\beta");
    }

    #[test]
    fn quadratic_formula_roundtrips() {
        let q = "x=\\frac{-b\\pm \\sqrt{b^{2}-4ac}}{2a}";
        assert_eq!(roundtrip(q), q);
    }
}
