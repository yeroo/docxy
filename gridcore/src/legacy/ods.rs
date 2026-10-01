//! OpenDocument Spreadsheet `.ods`: `content.xml` (the tables, their
//! cells and names) and `styles.xml` (data styles), read with
//! [`opccore::xml`].
//!
//! Formulas are OpenFormula (`of:=SUM([.A1:.A3];[Data.B1])`) as Excel and
//! LibreOffice write it, converted to Excel's syntax: `[.A1]` references,
//! `;` separators, `$$Name` names and `COM.MICROSOFT.` function prefixes.
//! Number formats are ODF data styles, converted to Excel format codes.
//!
//! Repeated rows and cells (`table:number-rows-repeated="1048000"` on the
//! padding Excel and LibreOffice write) are expanded only where they hold
//! something, so padding costs nothing. The copies a repeat that does hold
//! something adds are charged to a budget ([`super::Limits`]: copies, and
//! the text and formula bytes they hold) before they are made; a file that
//! asks for more is refused rather than allowed to exhaust memory. Text in
//! a cell's annotation or drawn shapes (`office:annotation`, `draw:*`) is
//! not the cell's value.

use std::collections::{BTreeMap, HashMap};

use opccore::xml::{Event, XmlParser};
use opccore::zip::ZipArchive;

use super::{BookIn, Limits, OpenError, SheetIn, sheet_prefix};
use crate::sheet::{Cell, CellValue, DefinedName, MAX_COLS, MAX_ROWS};

/// `2024-01-15` or `2024-01-15T12:30:00.5` as a serial.
fn parse_date(s: &str, date1904: bool) -> Option<f64> {
    let (date, time) = s.split_once('T').unwrap_or((s, ""));
    let mut it = date.splitn(3, '-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.get(..2)?.parse().ok()?;
    let mut secs = 0.0;
    if !time.is_empty() {
        let mut t = time.split(':');
        let h: f64 = t.next()?.parse().ok()?;
        let mi: f64 = t.next().unwrap_or("0").parse().ok()?;
        let sec = t.next().unwrap_or("0");
        // A zone suffix (Z, +01:00) has no meaning for a serial.
        let sec: f64 = sec
            .trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.')
            .parse()
            .ok()?;
        secs = h * 3600.0 + mi * 60.0 + sec;
    }
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(crate::sheet::parts_to_serial(y, m, d, 0, date1904) + secs / 86_400.0)
}

/// An ISO 8601 duration (`PT12H30M15S`) as a fraction of a day.
fn parse_duration(s: &str) -> Option<f64> {
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let s = s.strip_prefix('P')?;
    let (days, time) = s.split_once('T').unwrap_or((s, ""));
    let mut total = 0.0;
    if let Some(d) = days.strip_suffix('D') {
        total += d.parse::<f64>().ok()? * 86_400.0;
    }
    let mut num = String::new();
    for ch in time.chars() {
        match ch {
            'H' | 'M' | 'S' => {
                let v: f64 = num.parse().ok()?;
                num.clear();
                total += v * match ch {
                    'H' => 3600.0,
                    'M' => 60.0,
                    _ => 1.0,
                };
            }
            c => num.push(c),
        }
    }
    let day = total / 86_400.0;
    Some(if neg { -day } else { day })
}

fn decoded(raw: &str) -> String {
    let mut s = String::with_capacity(raw.len());
    XmlParser::append_decoded(raw, &mut s);
    s
}

// ---------------------------------------------------------------------------
// Formulas
// ---------------------------------------------------------------------------

/// One side of an ODF cell address (`$'My Sheet'.$A$1`, `.B2`, `.A`):
/// (sheet, the A1 part).
fn split_address(s: &str) -> Option<(Option<String>, String)> {
    let s = s.trim();
    let s = s.strip_prefix('$').unwrap_or(s);
    let (sheet, rest) = if let Some(q) = s.strip_prefix('\'') {
        // A quoted sheet name: '' is a quote.
        let mut name = String::new();
        let mut chars = q.char_indices().peekable();
        let mut end = None;
        while let Some((i, ch)) = chars.next() {
            if ch == '\'' {
                if chars.peek().map(|&(_, c)| c) == Some('\'') {
                    name.push('\'');
                    chars.next();
                } else {
                    end = Some(i + 1);
                    break;
                }
            } else {
                name.push(ch);
            }
        }
        (Some(name), &q[end?..])
    } else if let Some(dot) = s.rfind('.') {
        let sheet = &s[..dot];
        ((!sheet.is_empty()).then(|| sheet.to_string()), &s[dot..])
    } else {
        (None, s)
    };
    let cell = rest.strip_prefix('.').unwrap_or(rest);
    Some((sheet, cell.to_string()))
}

/// An ODF reference (`.A1`, `.A1:.B2`, `Data.A:.A`, `'Q3'.A1:.A1`,
/// `Data.$A$1:Data.$A$5`) in Excel's syntax.
fn convert_ref(r: &str) -> Option<String> {
    // The range's `:` is the first one outside a quoted sheet name, which
    // may hold one (`'A:B'.A1`). `''` inside a name closes and reopens it.
    let mut quoted = false;
    let colon = r.char_indices().find_map(|(i, c)| {
        match c {
            '\'' => quoted = !quoted,
            ':' if !quoted => return Some(i),
            _ => {}
        }
        None
    });
    let (first, second) = match colon {
        Some(i) => (&r[..i], Some(&r[i + 1..])),
        None => (r, None),
    };
    let (s1, c1) = split_address(first)?;
    let second = match second {
        Some(s) => Some(split_address(s)?),
        None => None,
    };
    if c1.contains("#REF!") || second.as_ref().is_some_and(|(_, c)| c.contains("#REF!")) {
        return Some("#REF!".to_string());
    }
    let first_sheet = s1.unwrap_or_default();
    match second {
        None => Some(format!("{}{c1}", sheet_prefix(&first_sheet, &first_sheet))),
        Some((s2, c2)) => {
            let last = s2.unwrap_or_else(|| first_sheet.clone());
            Some(format!("{}{c1}:{c2}", sheet_prefix(&first_sheet, &last)))
        }
    }
}

/// An OpenFormula expression (`of:=…`, the namespace prefix optional) in
/// Excel's syntax, without the `=`; `None` when it doesn't scan (an
/// unclosed string or reference).
pub(crate) fn convert_formula(f: &str) -> Option<String> {
    let body = match f.split_once(":=") {
        Some((ns, rest)) if !ns.contains(['"', '[']) => rest,
        _ => f.strip_prefix('=').unwrap_or(f),
    };
    let mut out = String::with_capacity(body.len());
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    let mut brace = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        match ch {
            '"' => {
                // A string literal, copied as is ("" is a quote).
                out.push('"');
                i += 1;
                loop {
                    let c = *chars.get(i)?;
                    out.push(c);
                    i += 1;
                    if c == '"' {
                        if chars.get(i) == Some(&'"') {
                            out.push('"');
                            i += 1;
                        } else {
                            break;
                        }
                    }
                }
                continue;
            }
            '[' => {
                // A reference; a quoted sheet name may hold `]`.
                let mut j = i + 1;
                let mut quoted = false;
                while j < chars.len() && (quoted || chars[j] != ']') {
                    if chars[j] == '\'' {
                        quoted = !quoted;
                    }
                    j += 1;
                }
                if j >= chars.len() {
                    return None;
                }
                let inner: String = chars[i + 1..j].iter().collect();
                out.push_str(&convert_ref(&inner)?);
                i = j + 1;
                continue;
            }
            '{' => brace += 1,
            '}' => brace = brace.saturating_sub(1),
            '$' if chars.get(i + 1) == Some(&'$') => {
                // `$$Name`: a named expression.
                i += 2;
                continue;
            }
            ';' => {
                out.push(',');
                i += 1;
                continue;
            }
            '#' => {
                // An error constant, whose `!` is no intersection.
                let rest: String = chars[i..].iter().take(8).collect();
                let lit = [
                    "#NULL!", "#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#NUM!", "#N/A",
                ]
                .into_iter()
                .find(|e| rest.to_ascii_uppercase().starts_with(e));
                if let Some(e) = lit {
                    out.push_str(e);
                    i += e.chars().count();
                    continue;
                }
            }
            '|' if brace > 0 => {
                out.push(';');
                i += 1;
                continue;
            }
            '~' => {
                // The ODF union operator.
                out.push(',');
                i += 1;
                continue;
            }
            '!' => {
                // The ODF intersection operator.
                out.push(' ');
                i += 1;
                continue;
            }
            c if c.is_alphabetic() || c == '_' => {
                // An identifier: a function name loses Excel's ODF namespace.
                let mut j = i;
                while j < chars.len()
                    && (chars[j].is_alphanumeric() || matches!(chars[j], '_' | '.'))
                {
                    j += 1;
                }
                let ident: String = chars[i..j].iter().collect();
                let name = ident
                    .strip_prefix("COM.MICROSOFT.")
                    .or_else(|| ident.strip_prefix("com.microsoft."))
                    .unwrap_or(&ident);
                if name.eq_ignore_ascii_case("SINGLE") && chars.get(j) == Some(&'(') {
                    out.push_str("_xlfn.SINGLE");
                } else {
                    out.push_str(name);
                }
                i = j;
                continue;
            }
            _ => {}
        }
        out.push(ch);
        i += 1;
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Number formats
// ---------------------------------------------------------------------------

/// The most digits a data style may ask for in any one place.
const MAX_DIGITS: u32 = 30;

/// One element of an ODF data style, in order.
#[derive(Debug, Clone)]
enum Part {
    /// `number:number`: (decimals, min decimals, min integer digits,
    /// grouping); `None` decimals means "as many as needed" (General).
    Number(Option<u32>, u32, u32, bool),
    Scientific(u32, u32, u32),
    Fraction(u32),
    Text(String),
    Currency(String, String),
    /// A date/time field and whether it is the long form.
    Field(&'static str, bool),
    /// `number:hours` with `truncate-on-overflow="false"`: elapsed hours.
    ElapsedHours(bool),
    /// Seconds with decimal places.
    Seconds(bool, u32),
    AmPm,
    TextContent,
    Month(bool, bool),
}

#[derive(Debug, Default, Clone)]
struct DataStyle {
    kind: String,
    parts: Vec<Part>,
    color: Option<String>,
    /// (condition, style) of each `style:map`.
    maps: Vec<(String, String)>,
}

/// An Excel color name for an ODF `fo:color`.
fn color_name(hex: &str) -> Option<&'static str> {
    Some(match hex.to_ascii_uppercase().as_str() {
        "#000000" => "Black",
        "#FFFFFF" => "White",
        "#FF0000" => "Red",
        "#00FF00" => "Green",
        "#0000FF" => "Blue",
        "#FFFF00" => "Yellow",
        "#FF00FF" => "Magenta",
        "#00FFFF" => "Cyan",
        _ => return None,
    })
}

/// The Windows LCID of a language/country pair, as `[$€-407]` spells it.
fn lcid(lang: &str, country: &str) -> Option<&'static str> {
    Some(match (lang, country) {
        ("en", "US") => "409",
        ("en", "GB") => "809",
        ("en", "CA") => "1009",
        ("en", "AU") => "C09",
        ("de", "DE") => "407",
        ("de", "AT") => "C07",
        ("de", "CH") => "807",
        ("fr", "FR") => "40C",
        ("fr", "CA") => "C0C",
        ("es", "ES") => "C0A",
        ("it", "IT") => "410",
        ("nl", "NL") => "413",
        ("pt", "BR") => "416",
        ("ja", "JP") => "411",
        ("zh", "CN") => "804",
        ("ru", "RU") => "419",
        _ => return None,
    })
}

/// Literal text in a format code: the characters Excel shows as they are
/// stay bare, a space or `-` is escaped (as Excel writes them), anything
/// else is quoted.
fn literal(text: &str) -> String {
    if text
        .chars()
        .all(|c| matches!(c, '/' | ':' | '%' | '(' | ')' | '$' | '+'))
    {
        return text.to_string();
    }
    if text
        .chars()
        .all(|c| matches!(c, ' ' | '-' | '/' | ':' | '%' | '(' | ')' | '$' | '+'))
    {
        return text
            .chars()
            .map(|c| match c {
                ' ' | '-' => format!("\\{c}"),
                c => c.to_string(),
            })
            .collect();
    }
    format!("\"{}\"", text.replace('"', "\\\""))
}

/// One section of a format code from a data style's own parts.
fn section(style: &DataStyle) -> Option<String> {
    let mut out = String::new();
    if let Some(c) = &style.color {
        out.push_str(&format!("[{c}]"));
    }
    let only_general = matches!(style.parts.as_slice(), [Part::Number(None, _, _, false)]);
    if only_general && style.kind == "number-style" {
        return Some(if out.is_empty() {
            "General".to_string()
        } else {
            format!("{out}General")
        });
    }
    for p in &style.parts {
        match p {
            Part::Number(dec, min_dec, min_int, group) => {
                let dec = dec.unwrap_or(*min_dec);
                let int = match (*group, *min_int) {
                    (true, 0) => "#,###".to_string(),
                    (true, n) => {
                        let zeros = "0".repeat(n as usize);
                        let pad = "#,##"
                            .chars()
                            .take(5usize.saturating_sub(zeros.len()))
                            .collect::<String>();
                        if zeros.len() >= 4 {
                            format!("#,{zeros}")
                        } else {
                            format!("{pad}{zeros}")
                        }
                    }
                    (false, 0) => "#".to_string(),
                    (false, n) => "0".repeat(n as usize),
                };
                out.push_str(&int);
                if dec > 0 {
                    out.push('.');
                    out.push_str(&"0".repeat((*min_dec).min(dec) as usize));
                    out.push_str(&"#".repeat(dec.saturating_sub(*min_dec) as usize));
                }
            }
            Part::Scientific(dec, min_int, exp) => {
                out.push_str(&"0".repeat((*min_int).max(1) as usize));
                if *dec > 0 {
                    out.push('.');
                    out.push_str(&"0".repeat(*dec as usize));
                }
                out.push_str("E+");
                out.push_str(&"0".repeat((*exp).max(1) as usize));
            }
            Part::Fraction(digits) => {
                let q = "?".repeat((*digits).max(1) as usize);
                out.push_str(&format!("# {q}/{q}"));
            }
            Part::Text(t) => out.push_str(&literal(t)),
            Part::Currency(sym, id) => {
                if id.is_empty() {
                    out.push_str(&format!("[${sym}]"));
                } else {
                    out.push_str(&format!("[${sym}-{id}]"));
                }
            }
            Part::Field(f, long) => {
                let s = match (*f, *long) {
                    ("d", true) => "dd",
                    ("d", false) => "d",
                    ("y", true) => "yyyy",
                    ("y", false) => "yy",
                    ("w", true) => "dddd",
                    ("w", false) => "ddd",
                    ("h", true) => "hh",
                    ("h", false) => "h",
                    ("m", true) => "mm",
                    ("m", false) => "m",
                    _ => return None,
                };
                out.push_str(s);
            }
            Part::ElapsedHours(long) => out.push_str(if *long { "[hh]" } else { "[h]" }),
            Part::Seconds(long, dec) => {
                out.push_str(if *long { "ss" } else { "s" });
                if *dec > 0 {
                    out.push('.');
                    out.push_str(&"0".repeat(*dec as usize));
                }
            }
            Part::AmPm => out.push_str("AM/PM"),
            Part::TextContent => out.push('@'),
            Part::Month(textual, long) => out.push_str(match (*textual, *long) {
                (true, true) => "mmmm",
                (true, false) => "mmm",
                (false, true) => "mm",
                (false, false) => "m",
            }),
        }
    }
    Some(out)
}

/// The Excel format code of data style `name`, with its `style:map`
/// conditions turned into sections: `value()>=0` (or `>0`) first, `<0`
/// next, and the style's own parts for the rest.
fn format_code(styles: &HashMap<String, DataStyle>, name: &str) -> Option<String> {
    let style = styles.get(name)?;
    let own = section(style)?;
    if style.maps.is_empty() {
        return Some(own);
    }
    let mapped = |pred: &dyn Fn(&str) -> bool| {
        style
            .maps
            .iter()
            .find(|(c, _)| pred(&c.replace(' ', "")))
            .and_then(|(_, s)| styles.get(s))
            .and_then(section)
    };
    let pos = mapped(&|c| c == "value()>=0" || c == "value()>0");
    let neg = mapped(&|c| c == "value()<0");
    Some(match (pos, neg) {
        (Some(p), Some(n)) => format!("{p};{n};{own}"),
        (Some(p), None) => format!("{p};{own}"),
        (None, Some(n)) => format!("{own};{n}"),
        (None, None) => own,
    })
}

/// The data styles and table-cell styles of a styles or content part.
#[derive(Default)]
struct StyleSheet {
    data: HashMap<String, DataStyle>,
    /// Cell style → (data style, parent style).
    cells: HashMap<String, (Option<String>, Option<String>)>,
}

impl StyleSheet {
    fn read(&mut self, xml: &str) {
        let mut p = XmlParser::new(xml);
        let mut cur: Option<(String, DataStyle)> = None;
        // The element whose text the current part collects.
        let mut text_into: Option<usize> = None;
        let mut text = String::new();
        loop {
            match p.next() {
                Event::Start => {
                    let name = p.name();
                    let attr = |a: &str| decoded(p.attr(a));
                    if let Some(kind) = name
                        .strip_prefix("number:")
                        .filter(|k| k.ends_with("-style"))
                    {
                        cur = Some((
                            attr("style:name"),
                            DataStyle {
                                kind: kind.to_string(),
                                ..DataStyle::default()
                            },
                        ));
                        continue;
                    }
                    if name == "style:style" && p.attr("style:family") == "table-cell" {
                        let opt = |a: &str| Some(attr(a)).filter(|s| !s.is_empty());
                        self.cells.insert(
                            attr("style:name"),
                            (opt("style:data-style-name"), opt("style:parent-style-name")),
                        );
                        continue;
                    }
                    let Some((_, style)) = cur.as_mut() else {
                        continue;
                    };
                    // Digit counts, bounded as Excel bounds decimals (30), so a
                    // hostile count can't size a format code.
                    let num = |a: &str| p.attr(a).parse::<u32>().ok().map(|n| n.min(MAX_DIGITS));
                    let long = p.attr("number:style") == "long";
                    let part = match name {
                        "number:number" => Some(Part::Number(
                            num("number:decimal-places"),
                            num("number:min-decimal-places")
                                .or(num("number:decimal-places"))
                                .unwrap_or(0),
                            num("number:min-integer-digits").unwrap_or(1),
                            p.attr("number:grouping") == "true",
                        )),
                        "number:scientific-number" => Some(Part::Scientific(
                            num("number:decimal-places").unwrap_or(0),
                            num("number:min-integer-digits").unwrap_or(1),
                            num("number:min-exponent-digits").unwrap_or(2),
                        )),
                        "number:fraction" => Some(Part::Fraction(
                            num("number:min-denominator-digits").unwrap_or(1),
                        )),
                        "number:text" => Some(Part::Text(String::new())),
                        "number:currency-symbol" => {
                            let id = lcid(p.attr("number:language"), p.attr("number:country"))
                                .unwrap_or_default();
                            Some(Part::Currency(String::new(), id.to_string()))
                        }
                        "number:day" => Some(Part::Field("d", long)),
                        "number:year" => Some(Part::Field("y", long)),
                        "number:day-of-week" => Some(Part::Field("w", long)),
                        "number:minutes" => Some(Part::Field("m", long)),
                        "number:month" => {
                            Some(Part::Month(p.attr("number:textual") == "true", long))
                        }
                        "number:hours" => {
                            if style.kind == "time-style"
                                && p.attr("number:truncate-on-overflow") == "false"
                            {
                                Some(Part::ElapsedHours(long))
                            } else {
                                Some(Part::Field("h", long))
                            }
                        }
                        "number:seconds" => Some(Part::Seconds(
                            long,
                            num("number:decimal-places").unwrap_or(0),
                        )),
                        "number:am-pm" => Some(Part::AmPm),
                        "number:text-content" => Some(Part::TextContent),
                        "style:text-properties" => {
                            style.color = color_name(p.attr("fo:color")).map(str::to_string);
                            None
                        }
                        "style:map" => {
                            style
                                .maps
                                .push((attr("style:condition"), attr("style:apply-style-name")));
                            None
                        }
                        _ => None,
                    };
                    if let Some(part) = part {
                        let collects = matches!(part, Part::Text(_) | Part::Currency(..));
                        style.parts.push(part);
                        if collects {
                            text_into = Some(style.parts.len() - 1);
                            text.clear();
                        }
                    }
                }
                Event::Text => {
                    if text_into.is_some() {
                        XmlParser::append_decoded(p.text(), &mut text);
                    }
                }
                Event::End => {
                    if let (Some(i), Some((_, style))) = (text_into, cur.as_mut()) {
                        match style.parts.get_mut(i) {
                            Some(Part::Text(t)) | Some(Part::Currency(t, _)) => {
                                *t = std::mem::take(&mut text);
                            }
                            _ => {}
                        }
                        text_into = None;
                    }
                    if p.name()
                        .strip_prefix("number:")
                        .is_some_and(|k| k.ends_with("-style"))
                    {
                        if let Some((name, style)) = cur.take() {
                            self.data.insert(name, style);
                        }
                    }
                }
                Event::Eof => break,
            }
        }
    }

    /// The format code of cell style `name`, through its parents.
    fn code(&self, name: &str) -> Option<String> {
        let mut at = name;
        for _ in 0..16 {
            let (data, parent) = self.cells.get(at)?;
            if let Some(d) = data {
                return format_code(&self.data, d);
            }
            at = parent.as_deref()?;
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// The cell being read.
#[derive(Default)]
struct CellIn {
    attrs: HashMap<String, String>,
    text: String,
    paragraphs: usize,
    repeat: u32,
}

/// Read an `.ods` package.
pub(crate) fn read(zip: &ZipArchive) -> Result<BookIn, OpenError> {
    let content = zip
        .read("content.xml")
        .ok_or_else(|| OpenError::Corrupt("no content.xml".into()))?;
    let content = String::from_utf8_lossy(&content);
    let mut styles = StyleSheet::default();
    if let Some(s) = zip.read("styles.xml") {
        styles.read(&String::from_utf8_lossy(&s));
    }
    styles.read(&content);
    read_content(&content, &styles, Limits::default())
}

/// The bytes a copy of `cell` costs the repeat budget: its text and formula.
fn copy_bytes(cell: &Cell) -> usize {
    let text = match &cell.value {
        CellValue::Text(s) | CellValue::Error(s) => s.len(),
        _ => 0,
    };
    1 + text + cell.formula.as_ref().map_or(0, String::len)
}

fn read_content(xml: &str, styles: &StyleSheet, limits: Limits) -> Result<BookIn, OpenError> {
    let mut book = BookIn::with_limits(limits);
    let mut codes: HashMap<String, u32> = HashMap::new();
    let mut p = XmlParser::new(xml);
    let mut stack: Vec<String> = Vec::new();
    // Per table: the column default styles as runs (first column, style).
    let mut col_styles: Vec<(u32, String)> = Vec::new();
    let mut next_col = 0u32;
    let mut row = 0u32;
    let mut col = 0u32;
    let mut row_repeat = 1u32;
    let mut row_style = String::new();
    // The current row's cells, before its repeat is known to apply.
    let mut row_cells: Vec<(u32, Cell)> = Vec::new();
    let mut cell: Option<CellIn> = None;
    // Depth inside an annotation or a drawn shape, whose text is not the
    // cell's.
    let mut hidden = 0usize;
    // (name, sheet scope, definition) of each named range/expression.
    let mut names: Vec<(String, Option<usize>, String)> = Vec::new();
    let mut null_date_1904 = false;

    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                let attr = |a: &str| decoded(p.attr(a));
                match name.as_str() {
                    "table:table" => {
                        book.push_sheet(SheetIn {
                            name: attr("table:name"),
                            cells: BTreeMap::new(),
                        })?;
                        col_styles.clear();
                        next_col = 0;
                        row = 0;
                    }
                    "table:table-column" => {
                        let n = p
                            .attr("table:number-columns-repeated")
                            .parse()
                            .unwrap_or(1u32);
                        col_styles.push((next_col, attr("table:default-cell-style-name")));
                        next_col = next_col.saturating_add(n);
                    }
                    "table:table-row" => {
                        col = 0;
                        row_cells.clear();
                        row_repeat = p
                            .attr("table:number-rows-repeated")
                            .parse()
                            .unwrap_or(1u32)
                            .max(1);
                        row_style = attr("table:default-cell-style-name");
                    }
                    "table:table-cell" | "table:covered-table-cell" => {
                        let mut c = CellIn {
                            repeat: p
                                .attr("table:number-columns-repeated")
                                .parse()
                                .unwrap_or(1u32)
                                .max(1),
                            ..CellIn::default()
                        };
                        if name == "table:table-cell" {
                            for a in p.attrs() {
                                c.attrs.insert(a.name.to_string(), decoded(a.value));
                            }
                        }
                        cell = Some(c);
                    }
                    n if n == "office:annotation" || n.starts_with("draw:") => hidden += 1,
                    "text:p" if hidden == 0 => {
                        if let Some(c) = cell.as_mut() {
                            if c.paragraphs > 0 {
                                c.text.push('\n');
                            }
                            c.paragraphs += 1;
                        }
                    }
                    "text:s" if hidden == 0 => {
                        if let Some(c) = cell.as_mut() {
                            let n = p.attr("text:c").parse().unwrap_or(1usize).min(1 << 12);
                            c.text.push_str(&" ".repeat(n));
                        }
                    }
                    "text:tab" if hidden == 0 => {
                        if let Some(c) = cell.as_mut() {
                            c.text.push('\t');
                        }
                    }
                    "text:line-break" if hidden == 0 => {
                        if let Some(c) = cell.as_mut() {
                            c.text.push('\n');
                        }
                    }
                    "table:null-date" => {
                        null_date_1904 = p.attr("table:date-value").starts_with("1904-01-01");
                    }
                    "table:named-range" | "table:named-expression" => {
                        // Inside a table: scoped to it.
                        let scope = stack
                            .iter()
                            .any(|e| e == "table:table")
                            .then(|| book.sheets.len().saturating_sub(1));
                        let def = if name == "table:named-range" {
                            convert_ref(&attr("table:cell-range-address"))
                        } else {
                            convert_formula(&attr("table:expression"))
                        };
                        if let Some(def) = def {
                            names.push((attr("table:name"), scope, def));
                        }
                    }
                    _ => {}
                }
                stack.push(name);
            }
            Event::Text => {
                if hidden == 0 && stack.iter().any(|e| e == "text:p") {
                    if let Some(c) = cell.as_mut() {
                        XmlParser::append_decoded(p.text(), &mut c.text);
                    }
                }
            }
            Event::End => {
                let name = stack.pop().unwrap_or_default();
                match name.as_str() {
                    n if n == "office:annotation" || n.starts_with("draw:") => {
                        hidden = hidden.saturating_sub(1);
                    }
                    "table:table-cell" | "table:covered-table-cell" => {
                        let Some(c) = cell.take() else { continue };
                        let repeat = c.repeat;
                        if let Some(value) = cell_value(&c, null_date_1904) {
                            let style_name = c
                                .attrs
                                .get("table:style-name")
                                .cloned()
                                .filter(|s| !s.is_empty())
                                .or_else(|| (!row_style.is_empty()).then(|| row_style.clone()))
                                .or_else(|| {
                                    col_styles
                                        .iter()
                                        .rev()
                                        .find(|(first, _)| *first <= col)
                                        .map(|(_, s)| s.clone())
                                        .filter(|s| !s.is_empty())
                                });
                            let style = match style_name {
                                Some(s) => *codes.entry(s.clone()).or_insert_with(|| {
                                    styles.code(&s).map_or(0, |code| book.format_index(&code))
                                }),
                                None => 0,
                            };
                            let formula = c
                                .attrs
                                .get("table:formula")
                                .and_then(|f| convert_formula(f));
                            let made = Cell {
                                value,
                                formula,
                                style,
                                ..Cell::default()
                            };
                            let last = col.saturating_add(repeat).min(MAX_COLS);
                            let n = last.saturating_sub(col) as usize;
                            let copies = n.saturating_sub(1);
                            book.charge_repeats(copies, copies.saturating_mul(copy_bytes(&made)))?;
                            book.charge_cells(n)?;
                            for cc in col..last {
                                row_cells.push((cc, made.clone()));
                            }
                        }
                        col = col.saturating_add(repeat);
                    }
                    "table:table-row" => {
                        if !row_cells.is_empty() && !book.sheets.is_empty() {
                            let last = row.saturating_add(row_repeat).min(MAX_ROWS);
                            // The first instance was charged cell by cell;
                            // each further row copies them all.
                            let extra = (last.saturating_sub(row) as usize).saturating_sub(1);
                            let row_bytes: usize =
                                row_cells.iter().map(|(_, c)| copy_bytes(c)).sum();
                            let copies = extra.saturating_mul(row_cells.len());
                            book.charge_repeats(copies, extra.saturating_mul(row_bytes))?;
                            book.charge_cells(copies)?;
                            if let Some(sheet) = book.sheets.last_mut() {
                                for r in row..last {
                                    for (cc, made) in &row_cells {
                                        sheet.cells.insert((r, *cc), made.clone());
                                    }
                                }
                            }
                        }
                        row = row.saturating_add(row_repeat);
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
        }
    }
    book.date1904 = null_date_1904;
    for (name, scope, formula) in names {
        book.names.push(DefinedName {
            name,
            scope,
            formula,
        });
    }
    Ok(book)
}

/// A cell's value from its `office:value-type` and value attributes (or its
/// text); `None` for an empty cell, which then takes no place in the model.
fn cell_value(c: &CellIn, date1904: bool) -> Option<CellValue> {
    let a = |k: &str| c.attrs.get(k).map(String::as_str);
    let text = || a("office:string-value").map_or_else(|| c.text.clone(), str::to_string);
    // LibreOffice marks an error result with calcext:value-type; Excel uses
    // its own office:value-type="error".
    if a("calcext:value-type") == Some("error") || a("office:value-type") == Some("error") {
        return Some(CellValue::Error(text()));
    }
    let value = match a("office:value-type") {
        Some("float" | "percentage" | "currency") => {
            CellValue::Number(a("office:value")?.trim().parse().ok()?)
        }
        Some("date") => CellValue::Number(parse_date(a("office:date-value")?, date1904)?),
        Some("time") => CellValue::Number(parse_duration(a("office:time-value")?)?),
        Some("boolean") => CellValue::Bool(matches!(a("office:boolean-value"), Some("true" | "1"))),
        Some("string") => CellValue::Text(text()),
        // No type: text without a value, or nothing.
        _ if a("table:formula").is_some() => CellValue::Text(text()),
        _ if !c.text.is_empty() => CellValue::Text(c.text.clone()),
        _ => return None,
    };
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(body: &str) -> String {
        format!(
            r#"<office:document-content xmlns:office="o" xmlns:table="t" xmlns:text="x" xmlns:number="n" xmlns:style="s"><office:automatic-styles/><office:body><office:spreadsheet>{body}</office:spreadsheet></office:body></office:document-content>"#
        )
    }

    fn read_str(body: &str) -> BookIn {
        let xml = content(body);
        let mut styles = StyleSheet::default();
        styles.read(&xml);
        read_content(&xml, &styles, Limits::default()).unwrap()
    }

    #[test]
    fn date_serials_match_excel() {
        let d = |s: &str, d1904| parse_date(s, d1904).unwrap();
        assert_eq!(d("1900-01-01", false), 1.0);
        assert_eq!(d("1900-02-28", false), 59.0);
        assert_eq!(d("1900-03-01", false), 61.0);
        assert_eq!(d("2024-01-15", false), 45306.0);
        assert_eq!(d("1904-01-01", true), 0.0);
        assert_eq!(d("2024-01-15T12:00:00", true), 45306.0 - 1462.0 + 0.5);
        assert_eq!(d("2024-01-15T00:00:00.5", false), 45306.0 + 0.5 / 86_400.0);
        assert_eq!(parse_date("2024-13-01", false), None);
        let half_past = parse_duration("PT12H30M00S").unwrap();
        assert!((half_past - 12.5 / 24.0).abs() < 1e-12);
        assert_eq!(parse_date("2024-01-15T06:00:00", false), Some(45306.25));
    }

    #[test]
    fn formulas_convert_to_excel_syntax() {
        let c = |f: &str| convert_formula(f);
        assert_eq!(
            c("of:=SUM([.A1:.B2];[Data.C3])").as_deref(),
            Some("SUM(A1:B2,Data!C3)")
        );
        assert_eq!(
            c("of:=[.$A$1]&\"a;[b]\"").as_deref(),
            Some("$A$1&\"a;[b]\"")
        );
        assert_eq!(c("of:=SUM([Data.A:.A])").as_deref(), Some("SUM(Data!A:A)"));
        assert_eq!(c("of:=[$'My Sheet'.B2]").as_deref(), Some("'My Sheet'!B2"));
        assert_eq!(
            c("of:=SUM(['Q1'.A1:'Q3'.A1])").as_deref(),
            Some("SUM('Q1:Q3'!A1:A1)")
        );
        assert_eq!(c("of:=$$TaxRate*100").as_deref(), Some("TaxRate*100"));
        // A `:` in a quoted table name is part of the name, spelled as the
        // marker `build` renames (#876).
        assert_eq!(c("of:=['A:B'.A1]").as_deref(), Some("'A\u{FDD0}B'!A1"));
        assert_eq!(
            c("of:=SUM([$'A:B'.A:.A])").as_deref(),
            Some("SUM('A\u{FDD0}B'!A:A)")
        );
        assert_eq!(
            c("of:=SUM(['A:B'.A1:'C'.A1])").as_deref(),
            Some("SUM('A\u{FDD0}B:C'!A1:A1)")
        );
        assert_eq!(
            c("of:=COM.MICROSOFT.SINGLE(COM.MICROSOFT.IFS([.A1]>1;1;TRUE();2))").as_deref(),
            Some("_xlfn.SINGLE(IFS(A1>1,1,TRUE(),2))")
        );
        assert_eq!(
            c("of:=COM.MICROSOFT.CEILING([.A1];0.5)").as_deref(),
            Some("CEILING(A1,0.5)")
        );
        assert_eq!(c("of:=SUM({1;2|3;4})").as_deref(), Some("SUM({1,2;3,4})"));
        assert_eq!(c("of:=[.#REF!]+1").as_deref(), Some("#REF!+1"));
        assert_eq!(
            c("of:=IFERROR(1/0;#DIV/0!)").as_deref(),
            Some("IFERROR(1/0,#DIV/0!)")
        );
        assert_eq!(c("of:=[.A1:.B2]![.B1:.C3]").as_deref(), Some("A1:B2 B1:C3"));
        assert_eq!(c("of:=\"open"), None);
        assert_eq!(c("of:=[.A1"), None);
    }

    #[test]
    fn cells_values_and_repeats() {
        let b = read_str(
            r##"<table:table table:name="S"><table:table-column table:number-columns-repeated="16384"/>
            <table:table-row><table:table-cell office:value-type="float" office:value="1.5"><text:p>1.5</text:p></table:table-cell>
              <table:table-cell office:value-type="string"><text:p>a<text:s text:c="2"/>b</text:p><text:p>c</text:p><office:annotation><text:p>note</text:p></office:annotation></table:table-cell>
              <table:table-cell office:value-type="boolean" office:boolean-value="true"/>
              <table:table-cell office:value-type="error" office:string-value="#DIV/0!" table:formula="of:=1/0"><text:p>#DIV/0!</text:p></table:table-cell>
              <table:table-cell table:number-columns-repeated="16380"/></table:table-row>
            <table:table-row table:number-rows-repeated="2"><table:table-cell office:value-type="float" office:value="7" table:number-columns-repeated="2"/></table:table-row>
            <table:table-row table:number-rows-repeated="1048570"><table:table-cell table:number-columns-repeated="16384"/></table:table-row>
            </table:table>"##,
        );
        let c = &b.sheets[0].cells;
        assert_eq!(b.sheets[0].name, "S");
        assert_eq!(c[&(0, 0)].value, CellValue::Number(1.5));
        assert_eq!(c[&(0, 1)].value, CellValue::Text("a  b\nc".into()));
        assert_eq!(c[&(0, 2)].value, CellValue::Bool(true));
        assert_eq!(c[&(0, 3)].value, CellValue::Error("#DIV/0!".into()));
        assert_eq!(c[&(0, 3)].formula.as_deref(), Some("1/0"));
        // The repeated content row and cell expand; the padding doesn't.
        assert_eq!(c[&(2, 1)].value, CellValue::Number(7.0));
        assert_eq!(c.len(), 8);
    }

    #[test]
    fn null_date_1904_and_names() {
        let b = read_str(
            r#"<table:calculation-settings><table:null-date table:date-value="1904-01-01"/></table:calculation-settings>
            <table:table table:name="Data"><table:table-row><table:table-cell office:value-type="date" office:date-value="1904-01-02"/></table:table-row>
            <table:named-expressions><table:named-expression table:name="Local" table:expression="of:=[.A1]*2"/></table:named-expressions></table:table>
            <table:named-expressions><table:named-range table:name="TheData" table:cell-range-address="Data.$A$1:Data.$A$5"/>
            <table:named-expression table:name="TaxRate" table:expression="of:=0.21"/></table:named-expressions>"#,
        );
        assert!(b.date1904);
        assert_eq!(b.sheets[0].cells[&(0, 0)].value, CellValue::Number(1.0));
        let n: Vec<_> = b
            .names
            .iter()
            .map(|n| (n.name.as_str(), n.scope, n.formula.as_str()))
            .collect();
        assert_eq!(
            n,
            [
                ("Local", Some(0), "A1*2"),
                ("TheData", None, "Data!$A$1:$A$5"),
                ("TaxRate", None, "0.21")
            ]
        );
    }

    #[test]
    fn data_styles_become_format_codes() {
        let xml = r##"<x>
          <number:number-style style:name="N0"><number:number number:min-integer-digits="1"/></number:number-style>
          <number:percentage-style style:name="P"><number:number number:decimal-places="2" number:min-decimal-places="2" number:min-integer-digits="1"/><number:text>%</number:text></number:percentage-style>
          <number:date-style style:name="D"><number:month number:style="long"/><number:text>/</number:text><number:day number:style="long"/><number:text>/</number:text><number:year/></number:date-style>
          <number:time-style style:name="T"><number:hours number:style="long"/><number:text>:</number:text><number:minutes number:style="long"/><number:text> </number:text><number:am-pm/></number:time-style>
          <number:currency-style style:name="CP"><number:currency-symbol number:language="en" number:country="US">$</number:currency-symbol><number:number number:decimal-places="2" number:min-decimal-places="2" number:min-integer-digits="1" number:grouping="true"/></number:currency-style>
          <number:currency-style style:name="C"><style:text-properties fo:color="#FF0000"/><number:text>-</number:text><number:currency-symbol number:language="en" number:country="US">$</number:currency-symbol><number:number number:decimal-places="2" number:min-decimal-places="2" number:min-integer-digits="1" number:grouping="true"/><style:map style:condition="value()&gt;=0" style:apply-style-name="CP"/></number:currency-style>
          <number:number-style style:name="F"><number:number number:decimal-places="0" number:min-integer-digits="1" number:grouping="true"/></number:number-style>
          <number:text-style style:name="X"><number:text-content/></number:text-style>
          <style:style style:name="Default" style:family="table-cell" style:data-style-name="N0"/>
          <style:style style:name="ce1" style:family="table-cell" style:parent-style-name="Default"/>
          <style:style style:name="ce2" style:family="table-cell" style:data-style-name="C"/>
        </x>"##;
        let mut s = StyleSheet::default();
        s.read(xml);
        let code = |n: &str| format_code(&s.data, n);
        assert_eq!(code("N0").as_deref(), Some("General"));
        assert_eq!(code("P").as_deref(), Some("0.00%"));
        assert_eq!(code("D").as_deref(), Some("mm/dd/yy"));
        assert_eq!(code("T").as_deref(), Some("hh:mm\\ AM/PM"));
        assert_eq!(
            code("C").as_deref(),
            Some("[$$-409]#,##0.00;[Red]\\-[$$-409]#,##0.00")
        );
        assert_eq!(code("F").as_deref(), Some("#,##0"));
        assert_eq!(code("X").as_deref(), Some("@"));
        assert_eq!(s.code("ce1").as_deref(), Some("General"));
        assert_eq!(s.code("ce2"), code("C"));
    }

    #[test]
    fn a_row_times_column_repeat_bomb_is_refused_quickly() {
        let xml = content(
            r#"<table:table table:name="S"><table:table-row table:number-rows-repeated="1048576"><table:table-cell office:value-type="float" office:value="1" table:number-columns-repeated="16384"/></table:table-row></table:table>"#,
        );
        let started = std::time::Instant::now();
        let err = read_content(&xml, &StyleSheet::default(), Limits::default())
            .err()
            .unwrap();
        assert!(err.to_string().contains("too many repeated cells"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        // Few copies of a big string: the byte budget, at the column level
        // (16,383 copies of 64 KB) and at the row level (4,999 rows of one).
        let big = "x".repeat(64 << 10);
        for (rows, cols) in [(244, 16_384), (5_000, 1)] {
            let xml = content(&format!(
                r#"<table:table table:name="S"><table:table-row table:number-rows-repeated="{rows}"><table:table-cell office:value-type="string" office:string-value="{big}" table:number-columns-repeated="{cols}"/></table:table-row></table:table>"#
            ));
            let started = std::time::Instant::now();
            let err = read_content(&xml, &StyleSheet::default(), Limits::default())
                .err()
                .unwrap();
            assert!(err.to_string().contains("too many repeated cells"), "{err}");
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
        }
        // A filled-down constant is fine.
        let ok = content(
            r#"<table:table table:name="S"><table:table-row table:number-rows-repeated="1000"><table:table-cell office:value-type="float" office:value="1" table:number-columns-repeated="3"/></table:table-row></table:table>"#,
        );
        assert_eq!(
            read_content(&ok, &StyleSheet::default(), Limits::default())
                .unwrap()
                .sheets[0]
                .cells
                .len(),
            3000
        );
    }

    /// The repeat budget charges only the copies repeats add: plain cells
    /// past it still load, and only the total-cell budget bounds them.
    #[test]
    fn plain_cells_are_charged_to_the_cell_budget_only() {
        let cells: String = (0..20)
            .map(|i| format!(r#"<table:table-cell office:value-type="float" office:value="{i}"/>"#))
            .collect();
        let xml = content(&format!(
            r#"<table:table table:name="S"><table:table-row>{cells}</table:table-row></table:table>"#
        ));
        let tight_repeats = Limits {
            repeat_cells: 5,
            ..Limits::default()
        };
        let book = read_content(&xml, &StyleSheet::default(), tight_repeats).unwrap();
        assert_eq!(book.sheets[0].cells.len(), 20);
        let tight_cells = Limits {
            cells: 10,
            ..Limits::default()
        };
        let err = read_content(&xml, &StyleSheet::default(), tight_cells)
            .err()
            .unwrap();
        assert!(err.to_string().contains("too many cells"), "{err}");
    }

    /// A shape anchored in a cell holds text that is not the cell's.
    #[test]
    fn drawn_shapes_text_is_not_the_cells() {
        let b = read_str(
            r#"<table:table table:name="S"><table:table-row>
              <table:table-cell><draw:custom-shape draw:name="s"><text:p>Note</text:p></draw:custom-shape></table:table-cell>
              <table:table-cell office:value-type="string"><text:p>own</text:p><draw:frame><draw:text-box><text:p>boxed</text:p></draw:text-box></draw:frame></table:table-cell>
            </table:table-row></table:table>"#,
        );
        let c = &b.sheets[0].cells;
        assert!(!c.contains_key(&(0, 0)));
        assert_eq!(c[&(0, 1)].value, CellValue::Text("own".into()));
    }

    /// LibreOffice allows sheet names past Excel's 31 characters: the import
    /// cuts the name, and the formula and named range that use it follow.
    #[test]
    fn a_long_sheet_name_is_cut_and_its_references_follow() {
        let long = "Quarterly figures for the region";
        assert_eq!(long.chars().count(), 32);
        let b = read_str(&format!(
            r#"<table:table table:name="{long}"><table:table-row><table:table-cell office:value-type="float" office:value="5"/></table:table-row></table:table>
            <table:table table:name="Calc"><table:table-row><table:table-cell office:value-type="float" office:value="0" table:formula="of:=['{long}'.A1]*2"/></table:table-row></table:table>
            <table:named-expressions><table:named-range table:name="TheVal" table:cell-range-address="'{long}'.$A$1"/>
            <table:named-expression table:name="Both" table:expression="of:=[$'{long}'.$A$1]~[$'{long}'.$B$1]"/></table:named-expressions>"#
        ));
        let mut pkg = b.build();
        let cut: String = long.chars().take(31).collect();
        let wb = &mut pkg.workbook;
        assert_eq!(wb.sheets[0].name, cut);
        let mut engine = crate::engine::Engine::new(wb);
        engine.recalc_all(wb);
        let a1 = wb.sheets[1].cell(0, 0).unwrap();
        assert_eq!(
            a1.formula.as_deref(),
            Some(format!("'{cut}'!A1*2").as_str())
        );
        assert_eq!(a1.value, CellValue::Number(10.0));
        assert_eq!(wb.defined_names[0].formula, format!("'{cut}'!$A$1"));
        // An ODF union (`~`) is a list of areas, each renamed.
        assert_eq!(
            wb.defined_names[1].formula,
            format!("'{cut}'!$A$1,'{cut}'!$B$1")
        );
    }

    /// A table named `A:B` (no Excel name holds a `:`) is renamed `A_B`,
    /// and its references follow: a whole column, a cell, a 3D span and a
    /// named range. One to a table the file doesn't have keeps its text
    /// (#876).
    #[test]
    fn a_table_named_with_a_colon_keeps_its_references() {
        let num = |v: u32| {
            format!(
                r#"<table:table-row><table:table-cell office:value-type="float" office:value="{v}"/></table:table-row>"#
            )
        };
        let f = |text: &str| {
            format!(
                r#"<table:table-row><table:table-cell office:value-type="float" office:value="0" table:formula="{text}"/></table:table-row>"#
            )
        };
        let b = read_str(&format!(
            r#"<table:table table:name="A:B">{}{}{}</table:table>
            <table:table table:name="C">{}</table:table>
            <table:table table:name="Calc">{}{}{}{}</table:table>
            <table:named-expressions><table:named-range table:name="Col" table:cell-range-address="$'A:B'.$A$1:.$A$3"/></table:named-expressions>"#,
            num(1),
            num(2),
            num(3),
            num(10),
            f("of:=SUM(['A:B'.A:.A])"),
            f("of:=['A:B'.A2]"),
            f("of:=SUM(['A:B'.A1:'C'.A1])"),
            f("of:=['X:Y'.A1]"),
        ));
        let mut pkg = b.build();
        let wb = &mut pkg.workbook;
        assert_eq!(wb.sheets[0].name, "A_B");
        let mut engine = crate::engine::Engine::new(wb);
        engine.recalc_all(wb);
        let calc = &wb.sheets[2];
        let got = |r: u32| {
            let c = calc.cell(r, 0).unwrap();
            (c.formula.clone().unwrap(), c.value.clone())
        };
        assert_eq!(got(0), ("SUM(A_B!A:A)".into(), CellValue::Number(6.0)));
        assert_eq!(got(1), ("A_B!A2".into(), CellValue::Number(2.0)));
        assert_eq!(got(2), ("SUM(A_B:C!A1)".into(), CellValue::Number(11.0)));
        assert_eq!(got(3).0, "'X:Y'!A1");
        assert_eq!(wb.defined_names[0].formula, "A_B!$A$1:$A$3");
    }

    #[test]
    fn huge_digit_counts_give_a_bounded_code() {
        let xml = r#"<x><number:number-style style:name="N"><number:number number:decimal-places="4000000000" number:min-decimal-places="4000000000" number:min-integer-digits="4000000000" number:grouping="true"/></number:number-style>
          <number:number-style style:name="E"><number:scientific-number number:decimal-places="4000000000" number:min-integer-digits="4000000000" number:min-exponent-digits="4000000000"/></number:number-style>
          <number:number-style style:name="F"><number:fraction number:min-denominator-digits="4000000000"/></number:number-style>
          <number:time-style style:name="T"><number:seconds number:decimal-places="4000000000"/></number:time-style></x>"#;
        let mut s = StyleSheet::default();
        s.read(xml);
        for n in ["N", "E", "F", "T"] {
            let code = format_code(&s.data, n).unwrap();
            assert!(code.len() < 200, "{n}: {}", code.len());
        }
    }

    #[test]
    fn leniency_bad_values_and_formulas() {
        // A float without a number, a date that isn't one, and a formula
        // that doesn't scan: the cell keeps what it can.
        let b = read_str(
            r#"<table:table table:name="S"><table:table-row>
              <table:table-cell office:value-type="float" office:value="x"/>
              <table:table-cell office:value-type="date" office:date-value="soon"/>
              <table:table-cell office:value-type="float" office:value="3" table:formula="of:=[.A1"/>
              <table:unknown-element/></table:table-row></table:table>"#,
        );
        let c = &b.sheets[0].cells;
        assert_eq!(c.len(), 1);
        assert_eq!(c[&(0, 2)].value, CellValue::Number(3.0));
        assert_eq!(c[&(0, 2)].formula, None);
    }

    #[test]
    fn opens_through_the_sniffer_and_saves_1904() {
        let xml = content(
            r#"<table:calculation-settings><table:null-date table:date-value="1904-01-01"/></table:calculation-settings><table:table table:name="D"><table:table-row><table:table-cell office:value-type="float" office:value="100"/></table:table-row></table:table>"#,
        );
        let zip = opccore::zipwrite::write_zip(&[
            (
                "mimetype".to_string(),
                b"application/vnd.oasis.opendocument.spreadsheet".to_vec(),
            ),
            ("content.xml".to_string(), xml.into_bytes()),
        ]);
        let (pkg, fmt) = super::super::open_workbook(&zip).unwrap();
        assert_eq!(fmt, super::super::SourceFormat::Ods);
        let back = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        assert!(back.workbook.date1904);
        assert_eq!(
            back.workbook.sheets[0].cell(0, 0).unwrap().value,
            CellValue::Number(100.0)
        );
    }
}
