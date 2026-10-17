//! Document property content controls (#768): Insert › Quick Parts ›
//! Document Property, and the Header & Footer tab's Document Info, put an
//! inline `w:sdt` bound to a core property in `docProps/core.xml`, as Word
//! does. The control loads as two `Inline::Raw` boundaries around its runs
//! (see `load::parse_inline_sdt`); this builds the same shape.

use crate::field::DocProps;
use crate::model::{Inline, Run, RunProps};

/// The core-properties part every document property control is bound to.
pub const CORE_STORE_ID: &str = "{6C3C8BC8-F283-45AE-878A-BAB7291924A1}";

/// The extended-properties part (`docProps/app.xml`): Company, Manager.
pub const EXTENDED_STORE_ID: &str = "{55AF091B-3C7A-41E3-B477-F2FDAA23CFDA}";

/// The namespace prefixes Word's controls declare: `ns0` Dublin Core, `ns1`
/// the core-properties namespace.
const PREFIX_MAPPINGS: &str = "xmlns:ns0='http://purl.org/dc/elements/1.1/' \
     xmlns:ns1='http://schemas.openxmlformats.org/package/2006/metadata/core-properties'";

/// The extended-properties controls declare `ns0` as that part's namespace.
const EXTENDED_PREFIX_MAPPINGS: &str =
    "xmlns:ns0='http://schemas.openxmlformats.org/officeDocument/2006/extended-properties'";

/// A document property Word's Document Property menu lists, in its order:
/// the core properties (`docProps/core.xml`) and, since #1433, Company and
/// Manager of the extended ones (`docProps/app.xml`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundProp {
    Author,
    Category,
    Comments,
    Company,
    Keywords,
    Manager,
    Status,
    Subject,
    Title,
}

impl BoundProp {
    pub const ALL: [BoundProp; 9] = [
        Self::Author,
        Self::Category,
        Self::Comments,
        Self::Company,
        Self::Keywords,
        Self::Manager,
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
            Self::Company => "Company",
            Self::Keywords => "Keywords",
            Self::Manager => "Manager",
            Self::Status => "Status",
            Self::Subject => "Subject",
            Self::Title => "Title",
        }
    }

    /// Whether the property lives in `docProps/app.xml`.
    fn is_extended(self) -> bool {
        matches!(self, Self::Company | Self::Manager)
    }

    /// The package part that holds the property.
    pub fn part_name(self) -> &'static str {
        if self.is_extended() {
            "docProps/app.xml"
        } else {
            "docProps/core.xml"
        }
    }

    /// The `w:storeItemID` of the property's data store.
    fn store_id(self) -> &'static str {
        if self.is_extended() {
            EXTENDED_STORE_ID
        } else {
            CORE_STORE_ID
        }
    }

    /// The bound element, as the control's XPath names it.
    fn xpath(self) -> String {
        let (root, el) = match self {
            Self::Author => ("ns1:coreProperties", "ns0:creator"),
            Self::Category => ("ns1:coreProperties", "ns1:category"),
            Self::Comments => ("ns1:coreProperties", "ns0:description"),
            Self::Keywords => ("ns1:coreProperties", "ns1:keywords"),
            Self::Status => ("ns1:coreProperties", "ns1:contentStatus"),
            Self::Subject => ("ns1:coreProperties", "ns0:subject"),
            Self::Title => ("ns1:coreProperties", "ns0:title"),
            Self::Company => ("ns0:Properties", "ns0:Company"),
            Self::Manager => ("ns0:Properties", "ns0:Manager"),
        };
        format!("/{root}[1]/{el}[1]")
    }

    /// The element's name in its part.
    fn part_element(self) -> &'static str {
        match self {
            Self::Author => "dc:creator",
            Self::Category => "cp:category",
            Self::Comments => "dc:description",
            Self::Company => "Company",
            Self::Keywords => "cp:keywords",
            Self::Manager => "Manager",
            Self::Status => "cp:contentStatus",
            Self::Subject => "dc:subject",
            Self::Title => "dc:title",
        }
    }

    /// The property a control's `w:dataBinding` names, if it is one of ours.
    fn from_binding(store_id: &str, xpath: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.store_id().eq_ignore_ascii_case(store_id) && p.xpath() == xpath)
    }

    /// The property's value in `props` (read from `docProps/core.xml` and
    /// `docProps/app.xml`, [`crate::field::parse_app_props`]).
    pub fn value(self, props: &DocProps) -> String {
        match self {
            Self::Author => &props.author,
            Self::Category => &props.category,
            Self::Comments => &props.comments,
            Self::Company => &props.company,
            Self::Keywords => &props.keywords,
            Self::Manager => &props.manager,
            Self::Status => &props.content_status,
            Self::Subject => &props.subject,
            Self::Title => &props.title,
        }
        .clone()
    }

    /// `xml` (the property's part) with the property set to `text`: its
    /// element's content replaced, or the element added before the root's
    /// close. `None` when `xml` has no root to add it to.
    pub fn write(self, xml: &str, text: &str) -> Option<String> {
        let name = self.part_element();
        let mut escaped = String::new();
        crate::serialize::esc_text(text, &mut escaped);
        let open = format!("<{name}");
        let mut from = 0;
        while let Some(at) = xml[from..].find(&open).map(|i| from + i) {
            from = at + open.len();
            if !xml[from..].starts_with(['>', '/', ' ', '\t', '\r', '\n']) {
                continue;
            }
            let gt = xml[from..].find('>').map(|e| from + e + 1)?;
            let close = format!("</{name}>");
            if xml[..gt].ends_with("/>") {
                return Some(format!(
                    "{}<{name}>{escaped}</{name}>{}",
                    &xml[..at],
                    &xml[gt..]
                ));
            }
            let end = xml[gt..].find(&close).map(|e| gt + e)?;
            return Some(format!("{}{escaped}{}", &xml[..gt], &xml[end..]));
        }
        let root_close = xml.rfind("</")?;
        Some(format!(
            "{}<{name}>{escaped}</{name}>{}",
            &xml[..root_close],
            &xml[root_close..]
        ))
    }
}

/// A document property control in a paragraph's content (#1433): the `open`
/// and `close` boundaries' indices, the editor offset `end` its text ends at, the property it is bound to and the text it holds (empty while it
/// shows its placeholder).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParaControl {
    pub(crate) open: usize,
    pub(crate) close: usize,
    pub(crate) end: usize,
    /// The control's `w:id`, which pairs it across edits.
    pub(crate) id: Option<i64>,
    pub(crate) prop: BoundProp,
    pub(crate) text: String,
    pub(crate) placeholder: bool,
}

/// The document property controls among `content`, in order.
pub(crate) fn scan_paragraph(content: &[Inline]) -> Vec<ParaControl> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut open: Option<(usize, BoundProp, bool, Option<i64>)> = None;
    let mut text = String::new();
    for (k, inline) in content.iter().enumerate() {
        match inline {
            Inline::Raw(raw) if open.is_none() && raw.ends_with("<w:sdtContent>") => {
                if let Some(prop) = binding_of(raw) {
                    open = Some((
                        k,
                        prop,
                        crate::sect::has_flag(raw, "w:showingPlcHdr"),
                        sdt_ids(raw).first().copied(),
                    ));
                    text.clear();
                }
            }
            Inline::Raw(raw) if open.is_some() && raw == crate::load::SDT_BLOCK_CLOSE => {
                let (o, prop, placeholder, id) = open.take().expect("checked");
                out.push(ParaControl {
                    open: o,
                    close: k,
                    end: at,
                    id,
                    prop,
                    text: if placeholder {
                        String::new()
                    } else {
                        std::mem::take(&mut text)
                    },
                    placeholder,
                });
            }
            Inline::Run(r) if open.is_some() => text.push_str(&r.text),
            _ => {}
        }
        at += crate::editor::inline_len(inline);
    }
    out
}

/// The property a control's opening boundary is bound to.
fn binding_of(open: &str) -> Option<BoundProp> {
    let at = open.find("<w:dataBinding")?;
    let el = &open[at..at + open[at..].find('>')?];
    BoundProp::from_binding(
        &crate::load::xml_attr_value(el, "w:storeItemID")?,
        &crate::load::xml_attr_value(el, "w:xpath")?,
    )
}

/// `content` with the control `c` showing `text` instead (its placeholder
/// when empty); true when anything changed. The text takes the properties of
/// the control's first run.
pub(crate) fn set_control_text(content: &mut Vec<Inline>, c: &ParaControl, text: &str) -> bool {
    if c.text == text {
        return false;
    }
    let props = content[c.open + 1..c.close]
        .iter()
        .find_map(|i| match i {
            Inline::Run(r) => Some(r.props.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let shown = if text.is_empty() {
        format!("[{}]", c.prop.label())
    } else {
        text.to_string()
    };
    if let Inline::Raw(open) = &mut content[c.open] {
        // The flag as the file has it (`w:val` and all) goes; an empty
        // control gets Word's bare one back.
        let bare = crate::sect::remove_element(open, "w:showingPlcHdr");
        *open = if text.is_empty() {
            bare.replacen("<w:dataBinding", "<w:showingPlcHdr/><w:dataBinding", 1)
        } else {
            bare
        };
    }
    content.splice(
        c.open + 1..c.close,
        [Inline::Run(Run { text: shown, props })],
    );
    true
}

/// Every content control `w:id` in `xml` (a part, or serialized blocks).
pub fn sdt_ids(xml: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<w:id ") {
        rest = &rest[at..];
        let end = rest.find('>').unwrap_or(rest.len());
        if let Some(v) = crate::load::xml_attr_value(&rest[..end], "w:val") {
            // Word's ids are signed 32-bit; anything else cannot meet ours.
            if let Ok(n) = v.trim().parse::<i32>() {
                out.push(i64::from(n));
            }
        }
        rest = &rest[end..];
    }
    out
}

/// `n` content-control ids none of `used` holds: the ones after the largest,
/// else (that would pass Word's signed 32-bit range) the smallest free
/// positive ones.
pub fn free_sdt_ids(used: &[i64], n: usize) -> Vec<i64> {
    let max = used.iter().copied().max().unwrap_or(0).max(0);
    let count = i64::try_from(n).unwrap_or(i64::MAX);
    if max
        .checked_add(count)
        .is_some_and(|last| last <= i64::from(i32::MAX))
    {
        return (1..=count).map(|k| max + k).collect();
    }
    let taken: std::collections::HashSet<i64> = used.iter().copied().collect();
    (1..).filter(|k| !taken.contains(k)).take(n).collect()
}

/// A content-control id no control in any story's XML in `xmls` has (Word
/// keeps them unique across the document and its headers and footers):
/// [`free_sdt_ids`] over their [`sdt_ids`].
pub fn next_control_id<'a>(xmls: impl IntoIterator<Item = &'a str>) -> i64 {
    let used: Vec<i64> = xmls.into_iter().flat_map(sdt_ids).collect();
    free_sdt_ids(&used, 1).first().copied().unwrap_or(1)
}

/// An inline document property control for `prop`, bound to it, with
/// control id `id`, showing `value` in runs of `props`; an empty value
/// shows Word's placeholder (`[Title]`) with `w:showingPlcHdr`. Word's own
/// `w:placeholder` names a glossary entry this package does not have, so
/// it is left out.
pub fn core_property_control(
    prop: BoundProp,
    value: &str,
    id: i64,
    props: &RunProps,
) -> Vec<Inline> {
    let alias = prop.label();
    let placeholder = value.is_empty();
    let text_el = if prop == BoundProp::Comments {
        "<w:text w:multiLine=\"1\"/>"
    } else {
        "<w:text/>"
    };
    let open = format!(
        "<w:sdt><w:sdtPr><w:alias w:val=\"{alias}\"/><w:id w:val=\"{id}\"/>{}\
         <w:dataBinding w:prefixMappings=\"{}\" \
         w:xpath=\"{}\" w:storeItemID=\"{}\"/>\
         {text_el}</w:sdtPr><w:sdtContent>",
        if placeholder {
            "<w:showingPlcHdr/>"
        } else {
            ""
        },
        if prop.is_extended() {
            EXTENDED_PREFIX_MAPPINGS
        } else {
            PREFIX_MAPPINGS
        },
        prop.xpath(),
        prop.store_id(),
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
        let c = core_property_control(
            BoundProp::Title,
            "Quarterly Report",
            5,
            &RunProps::default(),
        );
        // The serializer writes every `w:t` with `xml:space`; Word left it off.
        let xml = para_xml(c).replace(" xml:space=\"preserve\"", "");
        assert!(xml.contains(WORD_TITLE), "{xml}");
    }

    #[test]
    fn an_empty_property_shows_the_placeholder_and_each_binds_its_element() {
        let c = core_property_control(BoundProp::Status, "", 9, &RunProps::default());
        let xml = para_xml(c);
        assert!(xml.contains("<w:showingPlcHdr/>"), "{xml}");
        assert!(xml.contains(">[Status]<"), "{xml}");
        assert!(xml.contains("w:xpath=\"/ns1:coreProperties[1]/ns1:contentStatus[1]\""));
        let xpaths: Vec<String> = BoundProp::ALL
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
                "/ns0:Properties[1]/ns0:Company[1]",
                "/ns1:coreProperties[1]/ns1:keywords[1]",
                "/ns0:Properties[1]/ns0:Manager[1]",
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
        assert_eq!(BoundProp::Title.value(&props), "Q & A");
        assert_eq!(BoundProp::Author.value(&props), "Ann");
        assert_eq!(BoundProp::Category.value(&props), "Memo");
        assert_eq!(BoundProp::Status.value(&props), "");
        assert_eq!(BoundProp::Subject.value(&props), "");
        let props = crate::field::parse_core_props(
            "<x><cp:contentStatus>Final</cp:contentStatus><dc:subject/></x>",
        );
        assert_eq!(BoundProp::Status.value(&props), "Final");
        assert_eq!(BoundProp::Subject.value(&props), "");
    }

    #[test]
    fn control_ids_are_one_past_the_largest_in_any_story() {
        assert_eq!(next_control_id([]), 1);
        let a = "<w:sdtPr><w:id w:val=\"-1311087653\"/></w:sdtPr>";
        let b = "<w:sdtPr><w:id w:val=\"12\"/></w:sdtPr><w:footnote w:id=\"99999999999\"/>";
        assert_eq!(next_control_id([a, b]), 13);
        let full = "<w:id w:val=\"2147483647\"/><w:id w:val=\"1\"/>";
        assert_eq!(next_control_id([full]), 2);
        // FIX r2 m3: the i64 extremes neither overflow nor become the id.
        let extremes = "<w:id w:val=\"9223372036854775807\"/><w:id w:val=\"-9223372036854775808\"/>\
                        <w:id w:val=\"1\"/><w:id w:val=\"2\"/>";
        assert_eq!(next_control_id([extremes]), 3);
    }

    fn para(content: Vec<Inline>) -> Vec<Inline> {
        content
    }

    #[test]
    fn company_and_manager_bind_to_the_extended_store_1433() {
        let c = core_property_control(BoundProp::Company, "Acme & Co", 3, &RunProps::default());
        let xml = para_xml(c);
        assert!(
            xml.contains("w:xpath=\"/ns0:Properties[1]/ns0:Company[1]\""),
            "{xml}"
        );
        assert!(xml.contains("w:storeItemID=\"{55AF091B-3C7A-41E3-B477-F2FDAA23CFDA}\""));
        assert!(xml.contains("extended-properties"), "{xml}");
        assert!(!xml.contains("core-properties"), "{xml}");
        assert_eq!(BoundProp::Company.part_name(), "docProps/app.xml");
        assert_eq!(BoundProp::Title.part_name(), "docProps/core.xml");
        let app = "<Properties xmlns=\"x\"><Application>docxy</Application>\
            <Company>Acme &amp; Co</Company><Manager/></Properties>";
        let props = crate::field::parse_app_props("<x/>", app);
        assert_eq!(BoundProp::Company.value(&props), "Acme & Co");
        assert_eq!(BoundProp::Manager.value(&props), "");
        // Every property is found again from its own binding.
        for p in BoundProp::ALL {
            let c = core_property_control(p, "v", 1, &RunProps::default());
            let found = scan_paragraph(&para(c));
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].prop, p);
            assert_eq!(found[0].text, "v");
        }
    }

    #[test]
    fn writing_a_property_replaces_or_adds_its_element_1433() {
        let core = "<cp:coreProperties><dc:title>Old</dc:title><dc:creator/></cp:coreProperties>";
        let out = BoundProp::Title.write(core, "A & B <x>").unwrap();
        assert_eq!(
            out,
            "<cp:coreProperties><dc:title>A &amp; B &lt;x&gt;</dc:title><dc:creator/></cp:coreProperties>"
        );
        let props = crate::field::parse_core_props(&out);
        assert_eq!(BoundProp::Title.value(&props), "A & B <x>");
        let out = BoundProp::Author.write(core, "Ann").unwrap();
        assert!(out.contains("<dc:creator>Ann</dc:creator>"), "{out}");
        // Absent: added before the root's close.
        let out = BoundProp::Subject.write(core, "S").unwrap();
        assert!(
            out.ends_with("<dc:subject>S</dc:subject></cp:coreProperties>"),
            "{out}"
        );
        // A longer name the property's begins is not it.
        let app = "<Properties><CompanyX>no</CompanyX></Properties>";
        let out = BoundProp::Company.write(app, "Acme").unwrap();
        assert_eq!(
            out,
            "<Properties><CompanyX>no</CompanyX><Company>Acme</Company></Properties>"
        );
        assert_eq!(BoundProp::Company.write("", "x"), None);
    }

    #[test]
    fn the_placeholder_flag_is_read_by_its_value_1433() {
        let open = |flag: &str| {
            Inline::Raw(format!(
                "<w:sdt><w:sdtPr><w:alias w:val=\"Title\"/>{flag}<w:dataBinding \
                 w:prefixMappings=\"xmlns:ns0='http://purl.org/dc/elements/1.1/' \
                 xmlns:ns1='http://schemas.openxmlformats.org/package/2006/metadata/core-properties'\" \
                 w:xpath=\"/ns1:coreProperties[1]/ns0:title[1]\" \
                 w:storeItemID=\"{CORE_STORE_ID}\"/></w:sdtPr><w:sdtContent>"
            ))
        };
        let control = |flag: &str| {
            vec![
                open(flag),
                Inline::Run(Run {
                    text: "[Title]".into(),
                    props: RunProps::default(),
                }),
                Inline::Raw(crate::load::SDT_BLOCK_CLOSE.to_string()),
            ]
        };
        // `w:val="0"` is off: the text is a value, not a placeholder.
        let off = scan_paragraph(&control("<w:showingPlcHdr w:val=\"0\"/>"));
        assert!(!off[0].placeholder);
        assert_eq!(off[0].text, "[Title]");
        let on = scan_paragraph(&control("<w:showingPlcHdr w:val=\"1\"/>"));
        assert!(on[0].placeholder);
        assert_eq!(on[0].text, "");
        // Writing text drops the flag whatever its spelling; writing none
        // leaves exactly one bare flag.
        for flag in ["<w:showingPlcHdr w:val=\"1\"/>", "<w:showingPlcHdr/>", ""] {
            let mut c = control(flag);
            let found = scan_paragraph(&c).remove(0);
            assert!(set_control_text(&mut c, &found, "Hello"));
            let Inline::Raw(o) = &c[0] else { panic!() };
            assert!(!o.contains("showingPlcHdr"), "{o}");
            let found = scan_paragraph(&c).remove(0);
            assert!(set_control_text(&mut c, &found, ""));
            let Inline::Raw(o) = &c[0] else { panic!() };
            assert_eq!(o.matches("<w:showingPlcHdr/>").count(), 1, "{o}");
            assert_eq!(o.matches("showingPlcHdr").count(), 1, "{o}");
        }
    }
}
