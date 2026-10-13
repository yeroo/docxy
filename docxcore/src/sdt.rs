//! Document property content controls (#768): Insert › Quick Parts ›
//! Document Property, and the Header & Footer tab's Document Info, put an
//! inline `w:sdt` bound to a core property in `docProps/core.xml`, as Word
//! does. The control loads as two `Inline::Raw` boundaries around its runs
//! (see `load::parse_inline_sdt`); this builds the same shape.

use crate::field::DocProps;
use crate::model::{Inline, Run, RunProps};

/// The core-properties part every document property control is bound to.
pub const CORE_STORE_ID: &str = "{6C3C8BC8-F283-45AE-878A-BAB7291924A1}";

/// The namespace prefixes Word's controls declare: `ns0` Dublin Core, `ns1`
/// the core-properties namespace.
const PREFIX_MAPPINGS: &str = "xmlns:ns0='http://purl.org/dc/elements/1.1/' \
     xmlns:ns1='http://schemas.openxmlformats.org/package/2006/metadata/core-properties'";

/// A core property Word's Document Property menu lists, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreProp {
    Author,
    Category,
    Comments,
    Keywords,
    Status,
    Subject,
    Title,
}

impl CoreProp {
    pub const ALL: [CoreProp; 7] = [
        Self::Author,
        Self::Category,
        Self::Comments,
        Self::Keywords,
        Self::Status,
        Self::Subject,
        Self::Title,
    ];

    /// The menu label, which is also the control's `w:alias`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Author => "Author",
            Self::Category => "Category",
            Self::Comments => "Comments",
            Self::Keywords => "Keywords",
            Self::Status => "Status",
            Self::Subject => "Subject",
            Self::Title => "Title",
        }
    }

    /// The bound element of `cp:coreProperties`, as the control's XPath
    /// names it.
    fn xpath_element(self) -> &'static str {
        match self {
            Self::Author => "ns0:creator",
            Self::Category => "ns1:category",
            Self::Comments => "ns0:description",
            Self::Keywords => "ns1:keywords",
            Self::Status => "ns1:contentStatus",
            Self::Subject => "ns0:subject",
            Self::Title => "ns0:title",
        }
    }

    /// The property's value in `props` (read from `docProps/core.xml`).
    pub fn value(self, props: &DocProps) -> String {
        match self {
            Self::Author => &props.author,
            Self::Category => &props.category,
            Self::Comments => &props.comments,
            Self::Keywords => &props.keywords,
            Self::Status => &props.content_status,
            Self::Subject => &props.subject,
            Self::Title => &props.title,
        }
        .clone()
    }
}

/// The next free content-control id over every story's XML in `xmls`: one
/// past the largest magnitude of any `w:id` a control there has, Word's
/// negative ids included (Word keeps them unique
/// across the document and its headers and footers). The id is an
/// `xsd:int`: past `i32::MAX`, the smallest positive id not in use.
pub fn next_control_id<'a>(xmls: impl IntoIterator<Item = &'a str>) -> i64 {
    let mut used = std::collections::BTreeSet::new();
    for xml in xmls {
        let mut from = 0;
        while let Some(at) = xml[from..].find("<w:id ").map(|i| from + i) {
            from = at + 6;
            let end = xml[at..].find('>').map_or(xml.len(), |e| at + e);
            if let Some(v) = crate::load::xml_attr_value(&xml[at..end], "w:val") {
                used.insert(v.trim().parse::<i64>().unwrap_or(0));
            }
        }
    }
    let next = used
        .iter()
        .map(|v| v.unsigned_abs())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .filter(|n| *n <= i32::MAX as u64);
    match next {
        Some(n) => n as i64,
        None => (1..).find(|i| !used.contains(i)).unwrap_or(1),
    }
}

/// An inline document property control for `prop`, bound to it, with
/// control id `id`, showing `value` in runs of `props`; an empty value
/// shows Word's placeholder (`[Title]`) with `w:showingPlcHdr`. Word's own
/// `w:placeholder` names a glossary entry this package does not have, so
/// it is left out.
pub fn core_property_control(
    prop: CoreProp,
    value: &str,
    id: i64,
    props: &RunProps,
) -> Vec<Inline> {
    let alias = prop.label();
    let placeholder = value.is_empty();
    let text_el = if prop == CoreProp::Comments {
        "<w:text w:multiLine=\"1\"/>"
    } else {
        "<w:text/>"
    };
    let open = format!(
        "<w:sdt><w:sdtPr><w:alias w:val=\"{alias}\"/><w:id w:val=\"{id}\"/>{}\
         <w:dataBinding w:prefixMappings=\"{PREFIX_MAPPINGS}\" \
         w:xpath=\"/ns1:coreProperties[1]/{}[1]\" w:storeItemID=\"{CORE_STORE_ID}\"/>\
         {text_el}</w:sdtPr><w:sdtContent>",
        if placeholder {
            "<w:showingPlcHdr/>"
        } else {
            ""
        },
        prop.xpath_element(),
    );
    let shown = if placeholder {
        format!("[{alias}]")
    } else {
        value.to_string()
    };
    vec![
        Inline::Raw(open),
        Inline::Run(Run {
            text: shown,
            props: props.clone(),
        }),
        Inline::Raw(crate::load::SDT_BLOCK_CLOSE.to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, Paragraph};

    /// Word 2024's Insert › Quick Parts › Document Property › Title, its
    /// `w:sdtPr` as Word wrote it but the glossary placeholder and the id.
    const WORD_TITLE: &str = "<w:sdt><w:sdtPr><w:alias w:val=\"Title\"/><w:id w:val=\"5\"/><w:dataBinding w:prefixMappings=\"xmlns:ns0='http://purl.org/dc/elements/1.1/' xmlns:ns1='http://schemas.openxmlformats.org/package/2006/metadata/core-properties'\" w:xpath=\"/ns1:coreProperties[1]/ns0:title[1]\" w:storeItemID=\"{6C3C8BC8-F283-45AE-878A-BAB7291924A1}\"/><w:text/></w:sdtPr><w:sdtContent><w:r><w:t>Quarterly Report</w:t></w:r></w:sdtContent></w:sdt>";

    fn para_xml(content: Vec<Inline>) -> String {
        crate::serialize::blocks_to_xml(&[Block::Paragraph(Paragraph {
            props: Default::default(),
            content,
        })])
    }

    #[test]
    fn a_title_control_matches_words() {
        let c = core_property_control(CoreProp::Title, "Quarterly Report", 5, &RunProps::default());
        // The serializer writes every `w:t` with `xml:space`; Word left it off.
        let xml = para_xml(c).replace(" xml:space=\"preserve\"", "");
        assert!(xml.contains(WORD_TITLE), "{xml}");
    }

    #[test]
    fn an_empty_property_shows_the_placeholder_and_each_binds_its_element() {
        let c = core_property_control(CoreProp::Status, "", 9, &RunProps::default());
        let xml = para_xml(c);
        assert!(xml.contains("<w:showingPlcHdr/>"), "{xml}");
        assert!(xml.contains(">[Status]<"), "{xml}");
        assert!(xml.contains("w:xpath=\"/ns1:coreProperties[1]/ns1:contentStatus[1]\""));
        let xpaths: Vec<String> = CoreProp::ALL
            .iter()
            .map(|p| {
                let c = core_property_control(*p, "v", 1, &RunProps::default());
                let Inline::Raw(open) = &c[0] else { panic!() };
                crate::load::xml_attr_value(open, "w:xpath").unwrap()
            })
            .collect();
        assert_eq!(
            xpaths,
            [
                "/ns1:coreProperties[1]/ns0:creator[1]",
                "/ns1:coreProperties[1]/ns1:category[1]",
                "/ns1:coreProperties[1]/ns0:description[1]",
                "/ns1:coreProperties[1]/ns1:keywords[1]",
                "/ns1:coreProperties[1]/ns1:contentStatus[1]",
                "/ns1:coreProperties[1]/ns0:subject[1]",
                "/ns1:coreProperties[1]/ns0:title[1]",
            ]
        );
    }

    #[test]
    fn values_come_from_core_xml() {
        // A longer element the name begins is not it; a self-closing one is
        // empty.
        let core = "<cp:coreProperties xmlns:cp=\"c\" xmlns:dc=\"d\">\
            <dc:title>Q &amp; A</dc:title><cp:categoryX>No</cp:categoryX><cp:category>Memo</cp:category>\
            <cp:contentStatus/><dc:creator>Ann</dc:creator></cp:coreProperties>";
        let props = crate::field::parse_core_props(core);
        assert_eq!(CoreProp::Title.value(&props), "Q & A");
        assert_eq!(CoreProp::Author.value(&props), "Ann");
        assert_eq!(CoreProp::Category.value(&props), "Memo");
        assert_eq!(CoreProp::Status.value(&props), "");
        assert_eq!(CoreProp::Subject.value(&props), "");
        let props = crate::field::parse_core_props(
            "<x><cp:contentStatus>Final</cp:contentStatus><dc:subject/></x>",
        );
        assert_eq!(CoreProp::Status.value(&props), "Final");
        assert_eq!(CoreProp::Subject.value(&props), "");
    }

    #[test]
    fn control_ids_are_one_past_the_largest_magnitude() {
        assert_eq!(next_control_id([]), 1);
        let a = "<w:sdtPr><w:id w:val=\"-1311087653\"/></w:sdtPr>";
        let b = "<w:sdtPr><w:id w:val=\"12\"/></w:sdtPr><w:footnote w:id=\"99999999999\"/>";
        assert_eq!(next_control_id([a, b]), 1_311_087_654);
        let full = "<w:id w:val=\"2147483647\"/><w:id w:val=\"1\"/>";
        assert_eq!(next_control_id([full]), 2);
        // FIX r2 m3: the i64 extremes neither overflow nor become the id.
        let extremes = "<w:id w:val=\"9223372036854775807\"/><w:id w:val=\"-9223372036854775808\"/>\
                        <w:id w:val=\"1\"/><w:id w:val=\"2\"/>";
        assert_eq!(next_control_id([extremes]), 3);
    }
}
