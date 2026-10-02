//! Mail-merge fields: recognising them in a field's instruction, building
//! them, and evaluating them against a recipient.
//!
//! See ECMA-376 Part 1 §17.16.5 (MERGEFIELD, NEXT, MERGEREC, MERGESEQ) and
//! Word's ADDRESSBLOCK / GREETINGLINE fields, whose `\f` format uses
//! `<<…_CODE_…>>` groups.

use super::csv::{Recipients, name_key};
use crate::field::{apply_star, instr_of, split_switches};
use crate::model::{Inline, RunProps};

/// A field mail merge evaluates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeFieldKind {
    /// `MERGEFIELD Name [\b before] [\f after] [\m] [\* format]`.
    MergeField {
        name: String,
        /// `\b`: text put before a non-empty value.
        before: Option<String>,
        /// `\f`: text put after a non-empty value.
        after: Option<String>,
        /// `\m`: `name` is a Match Fields name (First Name, City, …).
        mapped: bool,
        /// `\*`: a text format (Upper, Lower, Caps, FirstCap, MERGEFORMAT).
        format: Option<String>,
    },
    /// `ADDRESSBLOCK [\f format] [\e country-to-omit] [\c 0|1|2]`.
    AddressBlock {
        format: Option<String>,
        exclude_country: Option<String>,
        /// `\c 0` leaves the company out; anything else shows it.
        company: bool,
    },
    /// `GREETINGLINE [\f format] [\e fallback]`.
    GreetingLine {
        format: Option<String>,
        fallback: Option<String>,
    },
    /// `NEXT`: the rest of this copy uses the next recipient.
    Next,
    /// `MERGEREC`: the record's number in the data source.
    MergeRec,
    /// `MERGESEQ`: the record's number among the merged ones.
    MergeSeq,
}

impl MergeFieldKind {
    /// The `«…»` text Word shows for this field outside preview.
    pub fn placeholder(&self) -> String {
        match self {
            MergeFieldKind::MergeField { name, .. } => format!("\u{AB}{name}\u{BB}"),
            MergeFieldKind::AddressBlock { .. } => "\u{AB}AddressBlock\u{BB}".into(),
            MergeFieldKind::GreetingLine { .. } => "\u{AB}GreetingLine\u{BB}".into(),
            MergeFieldKind::Next => "\u{AB}Next Record\u{BB}".into(),
            MergeFieldKind::MergeRec => "\u{AB}Merge Record #\u{BB}".into(),
            MergeFieldKind::MergeSeq => "\u{AB}Merge Sequence #\u{BB}".into(),
        }
    }
}

/// The mail-merge field `raw` (a `w:fldSimple` or collapsed complex field)
/// is, if any. A field nested in the instruction is never one.
pub fn field_kind(raw: &str) -> Option<MergeFieldKind> {
    if !might_be_merge_field(raw) {
        return None;
    }
    instr_kind(&instr_of(raw)?)
}

/// A cheap test that rules out most fields without parsing: every merge
/// field's keyword is in its XML, in any case. Used where a frame paints
/// every field, so it allocates nothing.
pub fn might_be_merge_field(raw: &str) -> bool {
    let raw = raw.as_bytes();
    [&b"MERGE"[..], b"ADDRESSBLOCK", b"GREETINGLINE", b"NEXT"]
        .iter()
        .any(|k| raw.windows(k.len()).any(|w| w.eq_ignore_ascii_case(k)))
}

/// The mail-merge field an (entity-decoded) instruction is, if any.
pub fn instr_kind(instr: &str) -> Option<MergeFieldKind> {
    let s = instr.trim();
    let name_end = s.find(char::is_whitespace).unwrap_or(s.len());
    let keyword = s[..name_end].to_ascii_uppercase();
    let rest = &s[name_end..];
    let (head, switches) = split_switches(rest);
    let switch = |c: char| {
        switches
            .iter()
            .find(|(l, _)| l.eq_ignore_ascii_case(&c))
            .map(|(_, a)| a.clone())
    };
    Some(match keyword.as_str() {
        "MERGEFIELD" => {
            let name = head.trim().trim_matches('"').trim().to_string();
            if name.is_empty() {
                return None;
            }
            MergeFieldKind::MergeField {
                name,
                before: switch('b'),
                after: switch('f'),
                mapped: switches.iter().any(|(l, _)| l.eq_ignore_ascii_case(&'m')),
                format: switch('*'),
            }
        }
        "ADDRESSBLOCK" => MergeFieldKind::AddressBlock {
            format: switch('f'),
            exclude_country: switch('e'),
            company: switch('c').is_none_or(|c| c.trim() != "0"),
        },
        "GREETINGLINE" => MergeFieldKind::GreetingLine {
            format: switch('f'),
            fallback: switch('e'),
        },
        "NEXT" => MergeFieldKind::Next,
        "MERGEREC" => MergeFieldKind::MergeRec,
        "MERGESEQ" => MergeFieldKind::MergeSeq,
        _ => return None,
    })
}

/// A field name as an instruction argument: quoted when it holds a space or
/// a quote-worthy character.
fn quote_name(name: &str) -> String {
    if name.chars().any(|c| c.is_whitespace() || c == '\\') {
        format!("\"{}\"", name.replace('"', ""))
    } else {
        name.to_string()
    }
}

/// A `MERGEFIELD` for column `name`, showing `«name»`, in `props`.
pub fn merge_field(name: &str, props: &RunProps) -> Inline {
    let kind = MergeFieldKind::MergeField {
        name: name.to_string(),
        before: None,
        after: None,
        mapped: false,
        format: None,
    };
    crate::field::fld_simple(
        &format!(" MERGEFIELD {} ", quote_name(name)),
        &kind.placeholder(),
        props,
    )
}

/// Word's default Address Block format: name, company, two street lines,
/// "City, State Postal" and the country.
pub const DEFAULT_ADDRESS_FORMAT: &str = "<<_TITLE0_ >><<_FIRST0_>><< _LAST0_>><< _SUFFIX0_>>\n\
     <<_COMPANY_\n>><<_STREET1_\n>><<_STREET2_\n>><<_CITY_>><<, _STATE_>><< _POSTAL_>><<\n_COUNTRY_>>";

/// Word's default Greeting Line format: `Dear First Last,`.
pub const DEFAULT_GREETING_FORMAT: &str = "<<_BEFORE_ Dear >><<_FIRST0_>><< _LAST0_>><<_AFTER_ ,>>";

/// Word's default greeting when a recipient has no name.
pub const DEFAULT_GREETING_FALLBACK: &str = "Dear Sir or Madam,";

/// An `ADDRESSBLOCK` field (Word's default format), in `props`.
pub fn address_block_field(include_company: bool, props: &RunProps) -> Inline {
    let instr = format!(" ADDRESSBLOCK \\c {} ", u8::from(include_company));
    let kind = MergeFieldKind::AddressBlock {
        format: None,
        exclude_country: None,
        company: include_company,
    };
    crate::field::fld_simple(&instr, &kind.placeholder(), props)
}

/// A `GREETINGLINE` field: `salutation` (Dear, To, …), the name format, the
/// closing punctuation and the greeting used when a recipient has no name.
pub fn greeting_line_field(
    salutation: &str,
    name: GreetingName,
    punctuation: &str,
    fallback: &str,
    props: &RunProps,
) -> Inline {
    let format = format!(
        "<<_BEFORE_ {salutation} >>{}<<_AFTER_ {punctuation}>>",
        name.format()
    );
    let instr = format!(
        " GREETINGLINE \\f \"{}\" \\e \"{}\" ",
        format.replace('"', ""),
        fallback.replace('"', "")
    );
    let kind = MergeFieldKind::GreetingLine {
        format: None,
        fallback: None,
    };
    crate::field::fld_simple(&instr, &kind.placeholder(), props)
}

/// The Greeting Line dialog's name formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GreetingName {
    /// "Joshua Randall Jr."-style: first and last.
    FirstLast,
    /// "Mr. Randall": title and last.
    TitleLast,
    /// "Joshua".
    First,
}

impl GreetingName {
    pub const ALL: [GreetingName; 3] = [Self::FirstLast, Self::TitleLast, Self::First];

    pub fn label(self) -> &'static str {
        match self {
            Self::FirstLast => "Joshua Randall",
            Self::TitleLast => "Mr. Randall",
            Self::First => "Joshua",
        }
    }

    fn format(self) -> &'static str {
        match self {
            Self::FirstLast => "<<_FIRST0_>><< _LAST0_>>",
            Self::TitleLast => "<<_TITLE0_>><< _LAST0_>>",
            Self::First => "<<_FIRST0_>>",
        }
    }
}

/// A rule field (Rules ▸ Next Record, Merge Record #, Merge Sequence #).
pub fn rule_field(kind: &MergeFieldKind, props: &RunProps) -> Option<Inline> {
    let instr = match kind {
        MergeFieldKind::Next => " NEXT ",
        MergeFieldKind::MergeRec => " MERGEREC ",
        MergeFieldKind::MergeSeq => " MERGESEQ ",
        _ => return None,
    };
    Some(crate::field::fld_simple(instr, &kind.placeholder(), props))
}

/// The address parts Match Fields maps to columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressField {
    Title,
    First,
    Middle,
    Last,
    Suffix,
    Nickname,
    Company,
    Address1,
    Address2,
    City,
    State,
    PostalCode,
    Country,
    Email,
}

impl AddressField {
    pub const ALL: [AddressField; 14] = [
        Self::Title,
        Self::First,
        Self::Middle,
        Self::Last,
        Self::Suffix,
        Self::Nickname,
        Self::Company,
        Self::Address1,
        Self::Address2,
        Self::City,
        Self::State,
        Self::PostalCode,
        Self::Country,
        Self::Email,
    ];

    /// The name Match Fields shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Title => "Courtesy Title",
            Self::First => "First Name",
            Self::Middle => "Middle Name",
            Self::Last => "Last Name",
            Self::Suffix => "Suffix",
            Self::Nickname => "Nickname",
            Self::Company => "Company",
            Self::Address1 => "Address 1",
            Self::Address2 => "Address 2",
            Self::City => "City",
            Self::State => "State",
            Self::PostalCode => "Postal Code",
            Self::Country => "Country or Region",
            Self::Email => "E-mail Address",
        }
    }

    /// Column names auto-match recognises for this part (compared with
    /// [`name_key`]: case-insensitive, `_` equal to a space).
    fn synonyms(self) -> &'static [&'static str] {
        match self {
            Self::Title => &["courtesy title", "title", "salutation", "prefix", "mr mrs"],
            Self::First => &["first name", "first", "firstname", "given name", "forename"],
            Self::Middle => &["middle name", "middle", "middlename", "middle initial"],
            Self::Last => &["last name", "last", "lastname", "surname", "family name"],
            Self::Suffix => &["suffix", "name suffix"],
            Self::Nickname => &["nickname", "nick name"],
            Self::Company => &["company", "company name", "organization", "organisation"],
            Self::Address1 => &[
                "address 1",
                "address1",
                "address line 1",
                "address",
                "street",
                "street address",
                "address line1",
            ],
            Self::Address2 => &["address 2", "address2", "address line 2", "address line2"],
            Self::City => &["city", "town", "locality"],
            Self::State => &["state", "province", "region", "county", "state province"],
            Self::PostalCode => &[
                "postal code",
                "zip",
                "zip code",
                "zipcode",
                "postcode",
                "post code",
                "postalcode",
            ],
            Self::Country => &["country or region", "country", "country region", "nation"],
            Self::Email => &["e-mail address", "email address", "email", "e-mail", "mail"],
        }
    }

    /// The `\f` format code for this part.
    fn from_code(code: &str) -> Option<AddressField> {
        Some(match code {
            "_TITLE0_" => Self::Title,
            "_FIRST0_" => Self::First,
            "_MIDDLE0_" => Self::Middle,
            "_LAST0_" => Self::Last,
            "_SUFFIX0_" => Self::Suffix,
            "_NICK0_" => Self::Nickname,
            "_COMPANY_" => Self::Company,
            "_STREET1_" => Self::Address1,
            "_STREET2_" => Self::Address2,
            "_CITY_" => Self::City,
            "_STATE_" => Self::State,
            "_POSTAL_" => Self::PostalCode,
            "_COUNTRY_" => Self::Country,
            "_EMAIL_" => Self::Email,
            _ => return None,
        })
    }

    fn from_label(name: &str) -> Option<AddressField> {
        let key = name_key(name);
        Self::ALL
            .into_iter()
            .find(|f| name_key(f.label()) == key || f.synonyms().iter().any(|s| name_key(s) == key))
    }
}

/// Match Fields: which column holds each address part.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldMap {
    columns: [Option<usize>; AddressField::ALL.len()],
}

impl FieldMap {
    /// Word's automatic matching: each part takes the first column whose name
    /// is one of its synonyms.
    pub fn auto(recipients: &Recipients) -> FieldMap {
        let mut map = FieldMap::default();
        for f in AddressField::ALL {
            map.columns[f as usize] = f.synonyms().iter().find_map(|s| {
                let key = name_key(s);
                recipients.headers.iter().position(|h| name_key(h) == key)
            });
        }
        map
    }

    pub fn get(&self, f: AddressField) -> Option<usize> {
        self.columns[f as usize]
    }

    pub fn set(&mut self, f: AddressField, column: Option<usize>) {
        self.columns[f as usize] = column;
    }
}

/// One recipient, as merge fields see it.
#[derive(Debug, Clone, Copy)]
pub struct MergeContext<'a> {
    pub recipients: &'a Recipients,
    pub map: &'a FieldMap,
    /// The recipient's row in the data source.
    pub row: usize,
    /// Its 1-based position among the merged records (`MERGESEQ`).
    pub seq: usize,
}

impl MergeContext<'_> {
    fn part(&self, f: AddressField) -> &str {
        self.map
            .get(f)
            .map_or("", |c| self.recipients.value(self.row, c).trim())
    }

    /// The column a MERGEFIELD names, through Match Fields when it is mapped
    /// (or names no column but is an address part).
    fn column_of(&self, name: &str, mapped: bool) -> Option<usize> {
        let by_map = || AddressField::from_label(name).and_then(|f| self.map.get(f));
        if mapped {
            by_map().or_else(|| self.recipients.column(name))
        } else {
            self.recipients.column(name).or_else(by_map)
        }
    }
}

/// A merge field's value for `ctx`'s recipient; lines of an address block
/// are separated by `\n`. `None` when a MERGEFIELD names no column (Check for
/// Errors reports it; a merge leaves it empty).
pub fn eval(kind: &MergeFieldKind, ctx: &MergeContext) -> Option<String> {
    Some(match kind {
        MergeFieldKind::MergeField {
            name,
            before,
            after,
            mapped,
            format,
        } => {
            let col = ctx.column_of(name, *mapped)?;
            let mut value = ctx.recipients.value(ctx.row, col).to_string();
            if value.is_empty() {
                return Some(value);
            }
            if let Some(fmt) = format {
                value = apply_star(&value, 0.0, fmt);
            }
            format!(
                "{}{value}{}",
                before.as_deref().unwrap_or(""),
                after.as_deref().unwrap_or("")
            )
        }
        MergeFieldKind::AddressBlock {
            format,
            exclude_country,
            company,
        } => {
            let text = apply_format(
                format.as_deref().unwrap_or(DEFAULT_ADDRESS_FORMAT),
                ctx,
                exclude_country.as_deref(),
                *company,
            );
            text.split(['\n', '\r'])
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        }
        MergeFieldKind::GreetingLine { format, fallback } => {
            let format = format.as_deref().unwrap_or(DEFAULT_GREETING_FORMAT);
            let named = format_codes(format)
                .filter_map(AddressField::from_code)
                .any(|f| !ctx.part(f).is_empty());
            if named {
                apply_format(format, ctx, None, true)
            } else {
                fallback
                    .clone()
                    .unwrap_or_else(|| DEFAULT_GREETING_FALLBACK.to_string())
            }
        }
        MergeFieldKind::Next => String::new(),
        MergeFieldKind::MergeRec => (ctx.row + 1).to_string(),
        MergeFieldKind::MergeSeq => ctx.seq.to_string(),
    })
}

/// The `_CODE_` in a `<<…>>` group, with the text before and after it.
fn split_group(group: &str) -> Option<(&str, &str, &str)> {
    let start = group.find('_')?;
    let len = group[start + 1..].find('_')? + 2;
    let code = &group[start..start + len];
    code[1..len - 1]
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        .then_some((&group[..start], code, &group[start + len..]))
}

fn format_codes(format: &str) -> impl Iterator<Item = &str> {
    format
        .split("<<")
        .skip(1)
        .filter_map(|g| split_group(g.split(">>").next()?).map(|(_, c, _)| c))
}

/// Expand an ADDRESSBLOCK / GREETINGLINE `\f` format: text outside `<<…>>`
/// is literal; a group shows its text around the code only when the code's
/// value is not empty. `_BEFORE_` / `_AFTER_` groups always show their text.
fn apply_format(
    format: &str,
    ctx: &MergeContext,
    exclude_country: Option<&str>,
    company: bool,
) -> String {
    let mut out = String::new();
    let mut rest = format;
    while let Some(open) = rest.find("<<") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find(">>") else {
            out.push_str(&rest[open..]);
            return out;
        };
        let group = &after[..close];
        rest = &after[close + 2..];
        let Some((pre, code, post)) = split_group(group) else {
            continue;
        };
        if code == "_BEFORE_" || code == "_AFTER_" {
            out.push_str(post.strip_prefix(' ').unwrap_or(post));
            continue;
        }
        let Some(f) = AddressField::from_code(code) else {
            continue;
        };
        let value = ctx.part(f);
        let excluded = (f == AddressField::Company && !company)
            || (f == AddressField::Country
                && exclude_country.is_some_and(|c| c.trim().eq_ignore_ascii_case(value)));
        if !value.is_empty() && !excluded {
            out.push_str(pre);
            out.push_str(value);
            out.push_str(post);
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Inline;

    fn raw_of(i: &Inline) -> &str {
        match i {
            Inline::Field { raw, .. } => raw,
            _ => panic!("not a field"),
        }
    }

    fn mf(name: &str) -> MergeFieldKind {
        MergeFieldKind::MergeField {
            name: name.into(),
            before: None,
            after: None,
            mapped: false,
            format: None,
        }
    }

    #[test]
    fn recognises_merge_fields_simple_and_complex() {
        let simple = "<w:fldSimple w:instr=\" MERGEFIELD City \\* MERGEFORMAT \"><w:r><w:t>\u{AB}City\u{BB}</w:t></w:r></w:fldSimple>";
        assert_eq!(
            field_kind(simple),
            Some(MergeFieldKind::MergeField {
                name: "City".into(),
                before: None,
                after: None,
                mapped: false,
                format: Some("MERGEFORMAT".into()),
            })
        );
        let complex = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
            <w:r><w:instrText xml:space=\"preserve\"> MERGEFIELD &quot;First Name&quot; \\b &quot;Dear &quot; \\f &quot;,&quot; \\m </w:instrText></w:r>\
            <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>\u{AB}First Name\u{BB}</w:t></w:r>\
            <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
        assert_eq!(
            field_kind(complex),
            Some(MergeFieldKind::MergeField {
                name: "First Name".into(),
                before: Some("Dear ".into()),
                after: Some(",".into()),
                mapped: true,
                format: None,
            })
        );
        for (instr, kind) in [
            (" NEXT ", MergeFieldKind::Next),
            (" MERGEREC ", MergeFieldKind::MergeRec),
            (" mergeseq ", MergeFieldKind::MergeSeq),
        ] {
            let raw = format!("<w:fldSimple w:instr=\"{instr}\"/>");
            assert_eq!(field_kind(&raw), Some(kind), "{instr}");
        }
        assert!(matches!(
            instr_kind(" ADDRESSBLOCK \\f \"<<_FIRST0_>>\" \\e \"USA\" "),
            Some(MergeFieldKind::AddressBlock { format: Some(f), exclude_country: Some(e), company: true })
                if f == "<<_FIRST0_>>" && e == "USA"
        ));
        assert!(matches!(
            instr_kind(" GREETINGLINE \\e \"Hello,\" "),
            Some(MergeFieldKind::GreetingLine { format: None, fallback: Some(f) }) if f == "Hello,"
        ));
    }

    #[test]
    fn other_and_nested_fields_are_not_merge_fields() {
        assert_eq!(field_kind("<w:fldSimple w:instr=\" PAGE \"/>"), None);
        assert_eq!(field_kind("<w:fldSimple w:instr=\" MERGEFIELD \"/>"), None);
        assert_eq!(
            field_kind("<w:fldSimple w:instr=\" NEXTIF a = b \"/>"),
            None
        );
        // A field nested in the instruction: instr_of gives None.
        let nested = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
            <w:r><w:instrText> MERGEFIELD </w:instrText></w:r>\
            <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText> REF x </w:instrText></w:r>\
            <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
            <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
        assert_eq!(field_kind(nested), None);
    }

    #[test]
    fn merge_field_builds_a_mergefield_shown_as_its_name() {
        let f = merge_field("First Name", &RunProps::default());
        let Inline::Field { text, .. } = &f else {
            panic!()
        };
        assert_eq!(text, "\u{AB}First Name\u{BB}");
        assert_eq!(
            field_kind(raw_of(&f)),
            Some(mf("First Name")),
            "{}",
            raw_of(&f)
        );
        assert!(raw_of(&f).contains("w:instr=\" MERGEFIELD &quot;First Name&quot; \""));
        let g = merge_field("City", &RunProps::default());
        assert!(raw_of(&g).contains("w:instr=\" MERGEFIELD City \""));
    }

    #[test]
    fn merge_field_survives_save_and_load() {
        use crate::model::{Block, Document, Paragraph};
        let bold = RunProps {
            bold: true,
            ..Default::default()
        };
        let doc = Document {
            body: vec![Block::Paragraph(Paragraph {
                content: vec![merge_field("City", &bold)],
                ..Default::default()
            })],
        };
        let xml = crate::serialize::document_to_xml(&doc);
        let back = crate::load::parse_document_xml(&xml, &Default::default());
        let Block::Paragraph(p) = &back.body[0] else {
            panic!()
        };
        let Inline::Field { raw, text } = &p.content[0] else {
            panic!("{:?}", p.content)
        };
        assert_eq!(text, "\u{AB}City\u{BB}");
        assert_eq!(field_kind(raw), Some(mf("City")));
        assert!(crate::load::field_result_props(raw).bold);
    }

    fn people() -> Recipients {
        Recipients::parse_csv(
            b"Title,First Name,LastName,Company,Street,City,State,ZIP,Country\n\
              Ms.,Jane,Doe,Acme,1 Main St,Springfield,IL,62701,United States\n\
              ,,,,9 Elm Rd,Leeds,,LS1,UK\n",
        )
        .unwrap()
    }

    #[test]
    fn auto_match_finds_synonyms() {
        let r = people();
        let m = FieldMap::auto(&r);
        assert_eq!(m.get(AddressField::Title), Some(0));
        assert_eq!(m.get(AddressField::First), Some(1));
        assert_eq!(m.get(AddressField::Last), Some(2));
        assert_eq!(m.get(AddressField::Address1), Some(4));
        assert_eq!(m.get(AddressField::PostalCode), Some(7));
        assert_eq!(m.get(AddressField::Address2), None);
    }

    #[test]
    fn mergefield_values_switches_and_missing_columns() {
        let r = people();
        let map = FieldMap::auto(&r);
        let ctx = MergeContext {
            recipients: &r,
            map: &map,
            row: 0,
            seq: 1,
        };
        assert_eq!(eval(&mf("city"), &ctx).as_deref(), Some("Springfield"));
        assert_eq!(eval(&mf("Nope"), &ctx), None);
        let k = instr_kind(" MERGEFIELD City \\b \"in \" \\f \"!\" \\* Upper ").unwrap();
        assert_eq!(eval(&k, &ctx).as_deref(), Some("in SPRINGFIELD!"));
        // `\m`: the Match Fields name.
        let k = instr_kind(" MERGEFIELD Last_Name \\m ").unwrap();
        assert_eq!(eval(&k, &ctx).as_deref(), Some("Doe"));
        // An empty value drops \b and \f.
        let ctx2 = MergeContext { row: 1, ..ctx };
        let k = instr_kind(" MERGEFIELD Company \\b \"at \" ").unwrap();
        assert_eq!(eval(&k, &ctx2).as_deref(), Some(""));
        assert_eq!(eval(&MergeFieldKind::MergeRec, &ctx2).as_deref(), Some("2"));
        assert_eq!(eval(&MergeFieldKind::MergeSeq, &ctx2).as_deref(), Some("1"));
        assert_eq!(eval(&MergeFieldKind::Next, &ctx2).as_deref(), Some(""));
    }

    #[test]
    fn address_block_joins_non_empty_lines() {
        let r = people();
        let map = FieldMap::auto(&r);
        let ab = instr_kind(" ADDRESSBLOCK ").unwrap();
        let ctx = MergeContext {
            recipients: &r,
            map: &map,
            row: 0,
            seq: 1,
        };
        assert_eq!(
            eval(&ab, &ctx).unwrap(),
            "Ms. Jane Doe\nAcme\n1 Main St\nSpringfield, IL 62701\nUnited States"
        );
        let ctx = MergeContext { row: 1, ..ctx };
        assert_eq!(eval(&ab, &ctx).unwrap(), "9 Elm Rd\nLeeds LS1\nUK");
        // \e omits the home country.
        let ab = instr_kind(" ADDRESSBLOCK \\e \"united states\" ").unwrap();
        let ctx = MergeContext { row: 0, ..ctx };
        assert!(!eval(&ab, &ctx).unwrap().contains("United States"));
    }

    #[test]
    fn greeting_line_and_its_fallback() {
        let r = people();
        let map = FieldMap::auto(&r);
        let gl = instr_kind(" GREETINGLINE ").unwrap();
        let ctx = MergeContext {
            recipients: &r,
            map: &map,
            row: 0,
            seq: 1,
        };
        assert_eq!(eval(&gl, &ctx).unwrap(), "Dear Jane Doe,");
        let ctx1 = MergeContext { row: 1, ..ctx };
        assert_eq!(eval(&gl, &ctx1).unwrap(), "Dear Sir or Madam,");
        // The field the dialog builds.
        let built = greeting_line_field(
            "To",
            GreetingName::TitleLast,
            ":",
            "To whom it may concern:",
            &RunProps::default(),
        );
        let kind = field_kind(raw_of(&built)).unwrap();
        assert_eq!(eval(&kind, &ctx).unwrap(), "To Ms. Doe:");
        assert_eq!(eval(&kind, &ctx1).unwrap(), "To whom it may concern:");
    }

    #[test]
    fn match_fields_overrides_the_mapping() {
        let r = people();
        let mut map = FieldMap::auto(&r);
        map.set(AddressField::First, Some(3)); // Company as the first name
        let gl = instr_kind(" GREETINGLINE ").unwrap();
        let ctx = MergeContext {
            recipients: &r,
            map: &map,
            row: 0,
            seq: 1,
        };
        assert_eq!(eval(&gl, &ctx).unwrap(), "Dear Acme Doe,");
    }

    #[test]
    fn address_block_field_without_company() {
        let r = people();
        let map = FieldMap::auto(&r);
        let ctx = MergeContext {
            recipients: &r,
            map: &map,
            row: 0,
            seq: 1,
        };
        let f = address_block_field(false, &RunProps::default());
        let kind = field_kind(raw_of(&f)).unwrap();
        assert!(!eval(&kind, &ctx).unwrap().contains("Acme"));
        let f = address_block_field(true, &RunProps::default());
        let kind = field_kind(raw_of(&f)).unwrap();
        assert!(eval(&kind, &ctx).unwrap().contains("Acme"));
    }
}
