//! Envelopes (Mailings ▸ Create ▸ Envelopes… ▸ Add to Document, and Start
//! Mail Merge ▸ Envelopes…): a section of its own at the start of the
//! document, sized as the envelope, with the return address at the top left
//! and the delivery address in a frame centred near the bottom, as Word's
//! `Envelope.Insert` builds it.

use crate::editor::Editor;
use crate::model::{Block, FramePr, Inline, ParProps, Paragraph, Run, RunProps};

/// An envelope size, landscape, in twips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeSize {
    pub name: &'static str,
    /// Width and height of the envelope as it lies (landscape).
    pub w: i32,
    pub h: i32,
    /// The printer paper code Word writes (`w:pgSz w:code`).
    pub code: i32,
}

/// The sizes the Envelope Options list offers. Size 10 (4 1/8 × 9 1/2 in) is
/// Word's default in the US.
pub const ENVELOPE_SIZES: [EnvelopeSize; 6] = [
    EnvelopeSize {
        name: "Size 10 (4 1/8 x 9 1/2 in)",
        w: 13680,
        h: 5940,
        code: 20,
    },
    EnvelopeSize {
        name: "Size 6 3/4 (3 5/8 x 6 1/2 in)",
        w: 9360,
        h: 5220,
        code: 38,
    },
    EnvelopeSize {
        name: "Monarch (3 7/8 x 7 1/2 in)",
        w: 10800,
        h: 5580,
        code: 37,
    },
    // ISO sizes: millimetres × 1440 / 25.4, rounded.
    EnvelopeSize {
        name: "DL (110 x 220 mm)",
        w: 12472,
        h: 6236,
        code: 27,
    },
    EnvelopeSize {
        name: "C5 (162 x 229 mm)",
        w: 12983,
        h: 9184,
        code: 28,
    },
    EnvelopeSize {
        name: "C6 (114 x 162 mm)",
        w: 9184,
        h: 6463,
        code: 31,
    },
];

/// What the Envelopes dialog adds.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvelopeSpec {
    pub size: EnvelopeSize,
    /// The delivery address, one paragraph per line (for a merge, an
    /// Address Block field, or empty to fill in later).
    pub delivery: Vec<Vec<Inline>>,
    /// The return address lines; none when Omit is checked.
    pub return_address: Vec<String>,
}

impl EnvelopeSpec {
    /// A spec from plain text: `delivery` and `return_address` split at line
    /// breaks.
    pub fn from_text(size: EnvelopeSize, delivery: &str, return_address: &str) -> EnvelopeSpec {
        EnvelopeSpec {
            size,
            delivery: lines(delivery).map(|l| vec![text_run(l)]).collect(),
            return_address: lines(return_address).map(str::to_string).collect(),
        }
    }
}

fn lines(s: &str) -> impl Iterator<Item = &str> {
    s.split(['\n', '\r'])
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
}

fn text_run(text: &str) -> Inline {
    Inline::Run(Run {
        text: text.to_string(),
        props: RunProps::default(),
    })
}

/// The delivery address frame (Word's Envelope Address style): up to 5 1/2 ×
/// 1 3/8 in, centred on the page, at the bottom, text not beside it.
pub fn delivery_frame(size: EnvelopeSize) -> FramePr {
    FramePr {
        w: Some((size.w - 2880).clamp(4320, 7920)),
        h: Some(1980),
        h_rule: Some("exact".into()),
        h_space: Some(180),
        wrap: Some("auto".into()),
        h_anchor: Some("page".into()),
        x_align: Some("center".into()),
        y_align: Some("bottom".into()),
        ..Default::default()
    }
}

/// The envelope section's own sectPr: the envelope size, landscape, with
/// Word's envelope margins, one column and no header/footer references (so
/// the letter's header does not print on the envelope).
pub fn envelope_sect_pr(size: EnvelopeSize) -> String {
    format!(
        "<w:sectPr><w:pgSz w:w=\"{}\" w:h=\"{}\" w:orient=\"landscape\" w:code=\"{}\"/>\
         <w:pgMar w:top=\"360\" w:right=\"720\" w:bottom=\"360\" w:left=\"576\" \
         w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/><w:cols w:space=\"720\"/></w:sectPr>",
        size.w, size.h, size.code
    )
}

/// The envelope's blocks: the return address paragraphs, then the framed
/// delivery address paragraphs (at least one, so there is somewhere to put
/// an Address Block).
pub fn envelope_blocks(spec: &EnvelopeSpec) -> Vec<Block> {
    let mut blocks: Vec<Block> = spec
        .return_address
        .iter()
        .map(|l| {
            Block::Paragraph(Paragraph {
                content: vec![text_run(l)],
                ..Default::default()
            })
        })
        .collect();
    let frame = delivery_frame(spec.size);
    let delivery = if spec.delivery.is_empty() {
        vec![Vec::new()]
    } else {
        spec.delivery.clone()
    };
    for content in delivery {
        blocks.push(Block::Paragraph(Paragraph {
            props: ParProps {
                frame: Some(frame.clone()),
                ..Default::default()
            },
            content,
        }));
    }
    blocks
}

/// Add the envelope to the start of the document as its own section, as one
/// undo step.
pub fn insert_envelope(editor: &mut Editor, spec: &EnvelopeSpec) {
    editor.insert_section_at_start(envelope_blocks(spec), envelope_sect_pr(spec.size));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Document, SectionProperties};
    use crate::package::{load_package, new_package, save_package};
    use crate::sect::SectionSetup;

    fn letter() -> Editor {
        let mut doc = Document {
            body: vec![Block::Paragraph(Paragraph {
                content: vec![text_run("Dear Jane,")],
                ..Default::default()
            })],
        };
        doc.set_trailing_section_properties(SectionProperties {
            raw: "<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId8\"/>\
                  <w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:titlePg/>\
                  <w:type w:val=\"continuous\"/></w:sectPr>"
                .into(),
            property_change: None,
        });
        Editor::new(doc)
    }

    #[test]
    fn envelope_is_a_new_first_section_in_one_undo_step() {
        let mut ed = letter();
        let spec = EnvelopeSpec::from_text(
            ENVELOPE_SIZES[0],
            "Jane Doe\n1 Main St\nSpringfield, IL",
            "Acme\n9 Elm Rd",
        );
        insert_envelope(&mut ed, &spec);
        let sections = ed.sections();
        assert_eq!(sections.len(), 2);
        let first = SectionSetup::parse(&sections[0]);
        assert_eq!((first.page.w, first.page.h), (13680, 5940));
        assert!(first.page.landscape);
        assert!(!sections[0].contains("headerReference"), "{}", sections[0]);
        assert!(!sections[0].contains("titlePg"), "{}", sections[0]);
        // The letter keeps its header and now starts on a new page.
        assert!(sections[1].contains("headerReference"), "{}", sections[1]);
        assert!(sections[1].contains("<w:titlePg/>"), "{}", sections[1]);
        assert!(!sections[1].contains("continuous"), "{}", sections[1]);
        let text: Vec<String> = ed.doc.body.iter().map(|b| b.plain_text()).collect();
        assert_eq!(
            text[..6],
            [
                "Acme",
                "9 Elm Rd",
                "Jane Doe",
                "1 Main St",
                "Springfield, IL",
                "Dear Jane,"
            ]
        );
        let framed = ed
            .doc
            .body
            .iter()
            .filter(|b| matches!(b, Block::Paragraph(p) if p.props.frame.is_some()))
            .count();
        assert_eq!(framed, 3);
        assert!(ed.undo());
        assert_eq!(ed.sections().len(), 1);
        assert_eq!(ed.doc.body[0].plain_text(), "Dear Jane,");
    }

    #[test]
    fn delivery_frame_survives_save() {
        let mut ed = letter();
        insert_envelope(
            &mut ed,
            &EnvelopeSpec::from_text(ENVELOPE_SIZES[3], "Jane", ""),
        );
        let saved = save_package(&new_package(ed.doc.clone()));
        let xml = load_package(&saved)
            .unwrap()
            .part_text("word/document.xml")
            .unwrap();
        assert!(
            xml.contains(
                "<w:framePr w:w=\"7920\" w:h=\"1980\" w:hSpace=\"180\" w:wrap=\"auto\" \
                 w:hAnchor=\"page\" w:xAlign=\"center\" w:yAlign=\"bottom\" w:hRule=\"exact\"/>"
            ),
            "{xml}"
        );
        assert!(
            xml.contains("w:w=\"12472\" w:h=\"6236\" w:orient=\"landscape\""),
            "{xml}"
        );
    }

    #[test]
    fn an_empty_delivery_address_still_gets_a_frame() {
        let blocks = envelope_blocks(&EnvelopeSpec::from_text(ENVELOPE_SIZES[0], "", ""));
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], Block::Paragraph(p) if p.props.frame.is_some()));
    }
}
