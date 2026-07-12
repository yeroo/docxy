//! Generate `assets/sample.docx` — a feature-showcase document used for the
//! README screenshot and as a quick smoke-test file.
//!
//! Run from the workspace root:
//!
//! ```sh
//! cargo run -p docxcore --example gen_sample           # writes assets/sample.docx
//! cargo run -p docxcore --example gen_sample out.docx  # writes out.docx
//! ```
//!
//! The document is built directly against [`docxcore::model`] so it can show off
//! formatting Markdown can't express (text colour, highlight, centred runs,
//! merged table cells) alongside the styles `new_markdown_package` defines
//! (Title, Heading1–6, Quote, SourceCode) and its bullet/decimal numbering.

use docxcore::model::*;
use docxcore::package::{new_markdown_package, save_package};

/// A plain run.
fn run(text: &str) -> Inline {
    Inline::Run(Run {
        text: text.into(),
        props: RunProps::default(),
    })
}

/// A run carrying explicit properties (built via the `props!` shorthand below).
fn styled(text: &str, props: RunProps) -> Inline {
    Inline::Run(Run {
        text: text.into(),
        props,
    })
}

/// Terse `RunProps` literal: `props!(bold: true, color: "C00000")`.
macro_rules! props {
    ($($field:ident : $value:expr),* $(,)?) => {{
        #[allow(clippy::needless_update)]
        RunProps { $($field: prop_val!($field, $value),)* ..RunProps::default() }
    }};
}
/// Wrap `Option`/`String` fields so call sites can pass bare literals.
macro_rules! prop_val {
    (color, $v:expr) => {
        Some($v.to_string())
    };
    (highlight, $v:expr) => {
        Some($v.to_string())
    };
    (style_id, $v:expr) => {
        Some($v.to_string())
    };
    ($_f:ident, $v:expr) => {
        $v
    };
}

/// A paragraph from a list of inlines, with the given properties.
fn para(props: ParProps, content: Vec<Inline>) -> Block {
    Block::Paragraph(Paragraph { props, content })
}

/// A heading paragraph (renders bold; `Heading{level}` style for Word).
fn heading(level: u8, text: &str) -> Block {
    para(
        ParProps {
            heading_level: Some(level),
            style_id: Some(format!("Heading{level}")),
            ..Default::default()
        },
        vec![run(text)],
    )
}

/// A list item paragraph: `num_id` 1 = bullets, 2 = decimal (per the Markdown
/// numbering part `new_markdown_package` ships).
fn list_item(num_id: i32, content: Vec<Inline>) -> Block {
    para(
        ParProps {
            num_id: Some(num_id),
            ..Default::default()
        },
        content,
    )
}

/// A Mermaid diagram inline (a native Word DrawingML drawing).
fn mermaid(src: &str) -> Inline {
    let (raw, text) = docxcore::mermaid::to_drawing(src);
    Inline::SmartArt { raw, text }
}

/// A math equation inline from LaTeX (`display` selects block-style `oMathPara`).
fn math(latex: &str, display: bool) -> Inline {
    let raw = docxcore::latex::latex_to_omml(latex, display);
    let text = docxcore::omath::render_omath(&raw);
    Inline::Equation {
        raw,
        text,
        latex: Some(latex.to_string()),
    }
}

/// A single-paragraph table cell.
fn cell(span: u32, content: Vec<Inline>) -> Cell {
    Cell {
        grid_span: span,
        v_merge: VMerge::None,
        blocks: vec![para(ParProps::default(), content)],
        ..Default::default()
    }
}

fn build() -> Document {
    let center = ParProps {
        align: Align::Center,
        ..Default::default()
    };

    let mut body: Vec<Block> = Vec::new();

    // --- Title block --------------------------------------------------------
    body.push(para(
        ParProps {
            style_id: Some("Title".into()),
            align: Align::Center,
            ..Default::default()
        },
        vec![run("docxy")],
    ));
    body.push(para(
        center.clone(),
        vec![styled(
            "A fast terminal viewer & editor for Word .docx and Markdown",
            props!(italic: true, color: "767171"),
        )],
    ));
    body.push(para(
        ParProps {
            borders: ParBorders {
                bottom: Some(BorderKind::Single),
                ..Default::default()
            },
            ..Default::default()
        },
        vec![],
    ));

    // --- Intro --------------------------------------------------------------
    body.push(heading(1, "Welcome"));
    body.push(para(
        ParProps::default(),
        vec![
            run("Docxy opens real "),
            styled(".docx", props!(code: true)),
            run(" files right in your terminal — text, tables, lists, styles and "),
            run("images — and lets you "),
            styled("edit", props!(bold: true)),
            run(" and "),
            styled("save", props!(bold: true)),
            run(
                " them losslessly. It also speaks Markdown, and can export to PDF. \
                 Everything below is a single document rendered by docxy itself.",
            ),
        ],
    ));

    // --- Rich text ----------------------------------------------------------
    body.push(heading(2, "Rich text"));
    body.push(para(
        ParProps::default(),
        vec![
            run("Runs can be "),
            styled("bold", props!(bold: true)),
            run(", "),
            styled("italic", props!(italic: true)),
            run(", "),
            styled("underlined", props!(underline: true)),
            run(", "),
            styled("struck through", props!(strike: true)),
            run(", "),
            styled("coloured", props!(color: "C00000")),
            run(" "),
            styled("in", props!(color: "1F7A1F")),
            run(" "),
            styled("any", props!(color: "2E74B5")),
            run(" hue, "),
            styled("highlighted", props!(highlight: "yellow")),
            run(", or set as inline "),
            styled("code", props!(code: true)),
            run("."),
        ],
    ));

    // --- Lists --------------------------------------------------------------
    body.push(heading(2, "Lists"));
    body.push(para(ParProps::default(), vec![run("A bulleted list:")]));
    for it in [
        "Navigate with the arrow keys, Home/End and PgUp/PgDn",
        "Select with Shift+move; copy to the system clipboard",
        "Find & replace with Ctrl-F",
    ] {
        body.push(list_item(1, vec![run(it)]));
    }
    body.push(para(ParProps::default(), vec![run("…and a numbered one:")]));
    for it in [
        "Open a .docx or .md file",
        "Edit text, styles and tables",
        "Save in place, or Save As to convert formats",
    ] {
        body.push(list_item(2, vec![run(it)]));
    }

    // --- Tables (with a merged header) -------------------------------------
    body.push(heading(2, "Tables"));
    let bold = props!(bold: true);
    let table = Table {
        grid: vec![3600, 2400, 2400],
        rows: vec![
            // A single merged cell spanning all three columns.
            Row {
                cells: vec![cell(3, vec![styled("Format support", bold.clone())])],
                ..Default::default()
            },
            Row {
                cells: vec![
                    cell(1, vec![styled("Capability", bold.clone())]),
                    cell(1, vec![styled("docxy", bold.clone())]),
                    cell(1, vec![styled("plain editors", bold.clone())]),
                ],
                ..Default::default()
            },
            Row {
                cells: vec![
                    cell(1, vec![run("Styles & numbering")]),
                    cell(1, vec![styled("yes", props!(color: "1F7A1F"))]),
                    cell(1, vec![styled("no", props!(color: "C00000"))]),
                ],
                ..Default::default()
            },
            Row {
                cells: vec![
                    cell(1, vec![run("Merged cells")]),
                    cell(1, vec![styled("yes", props!(color: "1F7A1F"))]),
                    cell(1, vec![styled("no", props!(color: "C00000"))]),
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    body.push(Block::Table(table));

    // --- Quote & code -------------------------------------------------------
    body.push(heading(2, "Quotes & code"));
    body.push(para(
        ParProps {
            style_id: Some("Quote".into()),
            indent: 360,
            ..Default::default()
        },
        vec![run(
            "\u{201C}The terminal is the most powerful UI we have — \
             docxy just teaches it to read Word.\u{201D}",
        )],
    ));
    for line in [
        "fn main() {",
        "    let doc = docxcore::load::from_bytes(&bytes)?;",
        "    println!(\"{}\", doc.plain_text());",
        "}",
    ] {
        body.push(para(
            ParProps {
                style_id: Some("SourceCode".into()),
                ..Default::default()
            },
            vec![styled(line, props!(code: true))],
        ));
    }

    // --- Math (LaTeX → OMML) ------------------------------------------------
    body.push(heading(2, "Scientific formulas"));
    body.push(para(
        ParProps::default(),
        vec![
            run("Inline math like "),
            math("E=mc^2", false),
            run(" and "),
            math("\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}", false),
            run(" flow with the text."),
        ],
    ));
    body.push(para(ParProps::default(), vec![run("A display equation:")]));
    body.push(para(
        ParProps {
            align: Align::Center,
            ..Default::default()
        },
        vec![math("x = \\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}", true)],
    ));

    // --- Mermaid diagram (Mermaid → DrawingML) ------------------------------
    body.push(heading(2, "Diagrams"));
    body.push(para(
        ParProps::default(),
        vec![run("A Mermaid flowchart, rendered as native Word shapes:")],
    ));
    body.push(para(
        ParProps {
            align: Align::Center,
            ..Default::default()
        },
        vec![mermaid(
            "flowchart LR\n  A[Markdown] --> B[docxcore]\n  B --> C[Word .docx]\n  B --> D[PDF]",
        )],
    ));

    // --- Link & footer rule -------------------------------------------------
    body.push(heading(2, "Links"));
    body.push(para(
        ParProps::default(),
        vec![
            run("Project home: "),
            Inline::Hyperlink(Hyperlink {
                target: Some("https://github.com/yeroo/docxy".into()),
                anchor: None,
                rel_id: None,
                runs: vec![Run {
                    text: "github.com/yeroo/docxy".into(),
                    props: RunProps::default(),
                }],
            }),
        ],
    ));
    body.push(para(
        center,
        vec![styled(
            "Generated by docxcore — gen_sample example",
            props!(italic: true, color: "767171"),
        )],
    ));

    Document { body }
}

fn main() -> std::io::Result<()> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/sample.docx".to_string());
    if let Some(dir) = std::path::Path::new(&out).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let pkg = new_markdown_package(build());
    std::fs::write(&out, save_package(&pkg))?;
    println!("wrote {out}");
    Ok(())
}
