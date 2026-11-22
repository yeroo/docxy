//! OpenDocument Spreadsheet `.ods`: `content.xml` (the tables, their
//! cells and names, validations and conditional formats) and `styles.xml`
//! (data styles and cell styles), read with [`opccore::xml`].
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
//! asks for more is refused rather than allowed to exhaust memory. An
//! empty cell kept for its format has its own cap, past which it is left
//! out, never refused (#884). Text in a cell's annotation or drawn shapes
//! (`office:annotation`, `draw:*`) is not the cell's value; an annotation
//! is the cell's comment.

use std::collections::{BTreeMap, HashMap, HashSet};

use opccore::xml::{Event, XmlParser};
use opccore::zip::ZipArchive;

use super::{BookIn, ColIn, Limits, OpenError, SheetIn, sheet_prefix};
use crate::print::setup::{CellComments, Orientation, PageOrder, PageSetup};
use crate::sheet::{Cell, CellValue, DefinedName, MAX_COLS, MAX_ROWS, Xf};

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
        // The A1 part follows the name's `.`; anything else (an external
        // `'file:///b.ods'#$Sheet1.A1`) is no reference this reads.
        let rest = &q[end?..];
        if !(rest.is_empty() || rest.starts_with('.')) {
            return None;
        }
        (Some(name), rest)
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
    // Only an A1 shape is passed on: anything else would be stored as a
    // formula Excel can't read, so the cell keeps its value instead.
    if !a1_shaped(&c1) || second.as_ref().is_some_and(|(_, c)| !a1_shaped(c)) {
        return None;
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

/// Whether `c` is shaped like an A1 cell, column or row (`$A$1`, `A`,
/// `$3`): up to three letters then up to seven digits, either optionally
/// anchored, not both empty.
fn a1_shaped(c: &str) -> bool {
    let c = c.strip_prefix('$').unwrap_or(c);
    let letters = c.bytes().take_while(u8::is_ascii_alphabetic).count();
    let digits = &c[letters..];
    let digits = if letters > 0 {
        digits.strip_prefix('$').unwrap_or(digits)
    } else {
        digits
    };
    letters <= 3
        && digits.len() <= 7
        && digits.bytes().all(|b| b.is_ascii_digit())
        && letters + digits.len() > 0
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

/// The parts of a table-cell style's text and cell properties the model
/// holds (#884): each `None` where the style leaves it to its parent.
#[derive(Clone, Debug, Default, PartialEq)]
struct CellProps {
    bold: Option<bool>,
    italic: Option<bool>,
    /// `fo:color`: a colour, or `None` for one that doesn't read.
    color: Option<Option<(u8, u8, u8)>>,
    /// `fo:font-size`, in points.
    size: Option<f64>,
    /// The font family (`style:font-name` through the font-face
    /// declarations, or `fo:font-family`).
    font: Option<String>,
    /// `fo:background-color`: a colour, or `None` for `transparent`.
    fill: Option<Option<(u8, u8, u8)>>,
    /// Each side (top, bottom, left, right): a thin solid line.
    sides: [Option<bool>; 4],
    /// A diagonal line is drawn.
    diagonal: Option<bool>,
}

impl CellProps {
    /// These properties, each one this style leaves unset taken from
    /// `parent`.
    fn over(&self, parent: &CellProps) -> CellProps {
        let mut sides = self.sides;
        for (s, p) in sides.iter_mut().zip(parent.sides) {
            *s = s.or(p);
        }
        CellProps {
            bold: self.bold.or(parent.bold),
            italic: self.italic.or(parent.italic),
            color: self.color.or(parent.color),
            size: self.size.or(parent.size),
            font: self.font.clone().or_else(|| parent.font.clone()),
            fill: self.fill.or(parent.fill),
            sides,
            diagonal: self.diagonal.or(parent.diagonal),
        }
    }

    /// Read the attributes of a `style:text-properties` or
    /// `style:table-cell-properties` element.
    fn read(&mut self, p: &XmlParser, faces: &HashMap<String, String>) {
        let attr = |a: &str| Some(decoded(p.attr(a))).filter(|v| !v.is_empty());
        if let Some(w) = attr("fo:font-weight") {
            self.bold = Some(match w.as_str() {
                "bold" => true,
                "normal" => false,
                n => n.parse::<u32>().is_ok_and(|n| n >= 600),
            });
        }
        if let Some(st) = attr("fo:font-style") {
            self.italic = Some(matches!(st.as_str(), "italic" | "oblique"));
        }
        if let Some(c) = attr("fo:color") {
            self.color = Some(hex_color(&c));
        }
        if let Some(sz) = attr("fo:font-size") {
            if let Some(pt) = length_pt(&sz) {
                self.size = Some(pt);
            }
        }
        if let Some(face) = attr("style:font-name") {
            self.font = Some(faces.get(&face).cloned().unwrap_or(face));
        } else if let Some(family) = attr("fo:font-family") {
            self.font = Some(unquote(&family));
        }
        if let Some(bg) = attr("fo:background-color") {
            self.fill = Some(hex_color(&bg));
        }
        if let Some(b) = attr("fo:border") {
            self.sides = [Some(thin_line(&b)); 4];
        }
        for (i, side) in ["top", "bottom", "left", "right"].iter().enumerate() {
            if let Some(b) = attr(&format!("fo:border-{side}")) {
                self.sides[i] = Some(thin_line(&b));
            }
        }
        for d in ["style:diagonal-bl-tr", "style:diagonal-tl-br"] {
            if let Some(v) = attr(d) {
                self.diagonal = Some(self.diagonal == Some(true) || v != "none");
            }
        }
    }

    /// The model format these properties give, with number format `code`.
    fn xf(&self, code: Option<String>) -> Xf {
        let mut x = Xf::default();
        if code.is_some() {
            x.set_code(code);
        }
        x.bold = self.bold.unwrap_or(false);
        x.italic = self.italic.unwrap_or(false);
        // Black is the automatic colour as the model sees it.
        x.color = self.color.flatten().filter(|&c| c != (0, 0, 0));
        x.font_size = self.size.filter(|&s| s != 11.0);
        x.font_name = self
            .font
            .clone()
            .filter(|f| !f.is_empty() && !f.eq_ignore_ascii_case("Calibri"));
        x.fill = self.fill.flatten();
        x.border = self.sides == [Some(true); 4] && self.diagonal != Some(true);
        x
    }
}

/// `#rrggbb` as a colour; `None` for `transparent` or anything else.
fn hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

/// A font family as `svg:font-family` or `fo:font-family` spells it, its
/// quotes off (`'Liberation Sans'`).
fn unquote(s: &str) -> String {
    s.trim().trim_matches(|c| c == '\'' || c == '"').to_string()
}

/// An ODF length (`0.06pt`, `1.5875cm`, `0.889in`, `22.58mm`, `60px`) in
/// points.
pub(super) fn length_pt(s: &str) -> Option<f64> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (n, unit) = s.split_at(split);
    let n: f64 = n.trim().parse().ok()?;
    let pt = match unit {
        "pt" => n,
        "in" => n * 72.0,
        "cm" => n * 72.0 / 2.54,
        "mm" => n * 72.0 / 25.4,
        "pc" => n * 12.0,
        "px" => n * 0.75,
        _ => return None,
    };
    (pt.is_finite() && pt >= 0.0).then_some(pt)
}

/// Whether a border side (`0.06pt solid #000000`, `none`) is a thin solid
/// line: what the model's thin box holds. A line of 1pt or less is thin
/// (Excel writes its thin line as 0.06pt or 0.74pt, LibreOffice 0.74pt);
/// its colour doesn't matter, as the `.xls` reader keeps a coloured box.
fn thin_line(s: &str) -> bool {
    let mut solid = false;
    let mut width = None;
    for w in s.split_whitespace() {
        match w {
            "solid" => solid = true,
            "thin" => width = Some(0.75),
            "none" | "hidden" => return false,
            w => {
                if let Some(pt) = length_pt(w) {
                    width = Some(pt);
                }
            }
        }
    }
    solid && width.is_some_and(|pt| pt <= 1.0)
}

/// One table-cell style: its data style, its parent and its own
/// properties.
#[derive(Clone, Debug, Default)]
struct CellStyle {
    data: Option<String>,
    parent: Option<String>,
    props: CellProps,
    /// Its conditional styles (`style:map`): (condition, the style it
    /// applies, base cell).
    maps: Vec<(String, String, String)>,
}

/// A `style:page-layout` (#901): what its `style:page-layout-properties`
/// and header and footer styles say, lengths in points.
#[derive(Clone, Debug, Default)]
struct PageLayout {
    margin_left: Option<f64>,
    margin_right: Option<f64>,
    margin_top: Option<f64>,
    margin_bottom: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
    orientation: Option<Orientation>,
    /// `style:scale-to`'s percentage.
    scale: Option<u32>,
    /// `style:scale-to-X` / `-Y` (or `loext:`'s): pages wide and tall.
    fit_x: Option<u32>,
    fit_y: Option<u32>,
    over_then_down: bool,
    first_page: Option<u32>,
    centering: (bool, bool),
    /// `style:print`'s `grid`, `headers` and `annotations`.
    grid: bool,
    headings: bool,
    annotations: bool,
    /// The header's and footer's height plus their spacing from the body,
    /// when the layout styles them.
    header_room: Option<f64>,
    footer_room: Option<f64>,
}

impl PageLayout {
    /// `style:page-layout-properties`' attributes. A length or number that
    /// doesn't read, or is out of the model's range, is left out.
    fn read(&mut self, p: &XmlParser) {
        let len = |a: &str| length_in(p.attr(a));
        self.margin_left = len("fo:margin-left");
        self.margin_right = len("fo:margin-right");
        self.margin_top = len("fo:margin-top");
        self.margin_bottom = len("fo:margin-bottom");
        self.width = length_pt(p.attr("fo:page-width"));
        self.height = length_pt(p.attr("fo:page-height"));
        self.orientation = match p.attr("style:print-orientation") {
            "portrait" => Some(Orientation::Portrait),
            "landscape" => Some(Orientation::Landscape),
            _ => None,
        };
        self.scale = p
            .attr("style:scale-to")
            .trim()
            .strip_suffix('%')
            .and_then(|n| n.trim().parse::<f64>().ok())
            .filter(|n| n.fract() == 0.0)
            .map(|n| n as u32)
            .filter(|n| crate::print::setup::SCALE_RANGE.contains(n));
        let fit = |name: &str| {
            let v = match p.attr(&format!("style:{name}")) {
                "" => p.attr(&format!("loext:{name}")),
                v => v,
            };
            v.trim()
                .parse::<u32>()
                .ok()
                .filter(|&n| n <= crate::print::setup::FIT_MAX)
        };
        self.fit_x = fit("scale-to-X");
        self.fit_y = fit("scale-to-Y");
        self.over_then_down = p.attr("style:print-page-order") == "ltr";
        self.first_page = p
            .attr("style:first-page-number")
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|&n| n >= 1);
        self.centering = match p.attr("style:table-centering") {
            "horizontal" => (true, false),
            "vertical" => (false, true),
            "both" => (true, true),
            _ => (false, false),
        };
        for token in p.attr("style:print").split_whitespace() {
            match token {
                "grid" => self.grid = true,
                "headers" => self.headings = true,
                "annotations" => self.annotations = true,
                _ => {}
            }
        }
    }
}

/// One header or footer of a master page: present, shown (not
/// `style:display="false"`) and its text as an Excel header string.
#[derive(Clone, Debug, Default)]
struct HfPart {
    present: bool,
    shown: bool,
    text: Option<String>,
}

impl HfPart {
    /// Its text, when it is shown.
    fn shown_text(&self) -> Option<String> {
        (self.present && self.shown)
            .then(|| self.text.clone())
            .flatten()
    }
}

/// A `style:master-page`: its page layout and its header and footer parts
/// (header, footer, header-left, footer-left, header-first, footer-first).
#[derive(Clone, Debug, Default)]
struct MasterPage {
    layout: String,
    parts: [HfPart; 6],
}

/// The header or footer part `name` names: its index in
/// [`MasterPage::parts`].
fn hf_part(name: &str) -> Option<usize> {
    Some(match name {
        "style:header" => 0,
        "style:footer" => 1,
        "style:header-left" => 2,
        "style:footer-left" => 3,
        "style:header-first" | "loext:header-first" => 4,
        "style:footer-first" | "loext:footer-first" => 5,
        _ => return None,
    })
}

/// A header or footer part being read into Excel's header string: its
/// left, centre and right sections (paragraphs outside any region are the
/// centre's), the section being read, paragraphs per section, depth inside
/// a `text:p` and inside a field whose cached text is not kept.
#[derive(Default)]
struct HfText {
    sections: [String; 3],
    paragraphs: [usize; 3],
    region: Option<usize>,
    in_p: usize,
    skip: usize,
}

impl HfText {
    fn at(&mut self) -> &mut String {
        &mut self.sections[self.region.unwrap_or(1)]
    }

    /// An element starts inside the part.
    fn start(&mut self, name: &str, p: &XmlParser) {
        let field = |t: &str| Some(t.to_string());
        let code = match name {
            "style:region-left" => {
                self.region = Some(0);
                None
            }
            "style:region-center" => {
                self.region = Some(1);
                None
            }
            "style:region-right" => {
                self.region = Some(2);
                None
            }
            "text:p" => {
                let k = self.region.unwrap_or(1);
                if self.paragraphs[k] > 0 {
                    self.sections[k].push('\n');
                }
                self.paragraphs[k] += 1;
                self.in_p += 1;
                None
            }
            "text:s" => {
                let n = p.attr("text:c").parse().unwrap_or(1usize).min(255);
                Some(" ".repeat(n))
            }
            "text:tab" => field(" "),
            "text:line-break" => field("\n"),
            "text:page-number" => field("&P"),
            "text:page-count" => field("&N"),
            "text:sheet-name" => field("&A"),
            "text:date" => field("&D"),
            "text:time" => field("&T"),
            "text:file-name" => match p.attr("text:display") {
                "full" => field("&Z&F"),
                "path" => field("&Z"),
                _ => field("&F"),
            },
            _ => None,
        };
        if matches!(
            name,
            "text:page-number"
                | "text:page-count"
                | "text:sheet-name"
                | "text:date"
                | "text:time"
                | "text:file-name"
        ) {
            self.skip += 1;
        }
        if let Some(code) = code {
            if self.in_p > 0 {
                self.at().push_str(&code);
            }
        }
    }

    fn text(&mut self, raw: &str) {
        if self.in_p > 0 && self.skip == 0 {
            let mut t = String::new();
            XmlParser::append_decoded(raw, &mut t);
            let t = t.replace('&', "&&");
            self.at().push_str(&t);
        }
    }

    fn end(&mut self, name: &str) {
        match name {
            "style:region-left" | "style:region-center" | "style:region-right" => {
                self.region = None;
            }
            "text:p" => self.in_p = self.in_p.saturating_sub(1),
            "text:page-number" | "text:page-count" | "text:sheet-name" | "text:date"
            | "text:time" | "text:file-name" => self.skip = self.skip.saturating_sub(1),
            _ => {}
        }
    }

    /// Excel's header string: `&L`, `&C`, `&R` before each non-empty
    /// section; `None` when all are empty.
    fn finish(self) -> Option<String> {
        let mut out = String::new();
        for (code, text) in ["&L", "&C", "&R"].iter().zip(&self.sections) {
            if !text.is_empty() {
                out.push_str(code);
                out.push_str(text);
            }
        }
        super::page::hf_text(out)
    }
}

/// An ODF length in inches.
fn length_in(s: &str) -> Option<f64> {
    length_pt(s).map(|pt| pt / 72.0)
}

/// The data styles, table-cell styles and column styles of a styles or
/// content part, and (#901) its page layouts, master pages and the table,
/// row and column styles that place a sheet on a page.
#[derive(Default)]
struct StyleSheet {
    data: HashMap<String, DataStyle>,
    cells: HashMap<String, CellStyle>,
    /// `style:default-style style:family="table-cell"`: what every cell
    /// style inherits last.
    default_cell: CellProps,
    /// Font-face name → family (`office:font-face-decls`).
    faces: HashMap<String, String>,
    /// Column style → `style:column-width`, in points.
    columns: HashMap<String, f64>,
    /// `style:default-style style:family="table-column"`'s width.
    default_column: Option<f64>,
    /// Table style → its `style:master-page-name` (#901).
    table_masters: HashMap<String, String>,
    /// `office:master-styles`' master pages by name.
    masters: HashMap<String, MasterPage>,
    /// `style:page-layout`s by name.
    layouts: HashMap<String, PageLayout>,
    /// Row and column styles with `fo:break-before="page"`: a manual page
    /// break before each row (column) in them.
    break_rows: HashSet<String>,
    break_cols: HashSet<String>,
}

impl StyleSheet {
    fn read(&mut self, xml: &str) {
        let mut p = XmlParser::new(xml);
        let mut cur: Option<(String, DataStyle)> = None;
        // The table-cell style whose properties are being read: a named
        // one, or (`Some(None)`) the default style.
        let mut cell_style: Option<Option<String>> = None;
        // The table-column style being read (`Some(None)`: the default).
        let mut column_style: Option<Option<String>> = None;
        // The table-row style being read.
        let mut row_style: Option<String> = None;
        // The page layout being read, and whether inside its header style
        // (false) or footer style (true).
        let mut layout: Option<(String, PageLayout)> = None;
        let mut hf_style: Option<bool> = None;
        // The master page being read, and the header or footer part inside
        // it (its index, its depth and its text).
        let mut master: Option<(String, MasterPage)> = None;
        let mut part: Option<(usize, usize, HfText)> = None;
        let mut depth = 0usize;
        // The element whose text the current part collects.
        let mut text_into: Option<usize> = None;
        let mut text = String::new();
        loop {
            match p.next() {
                Event::Start => {
                    depth += 1;
                    let name = p.name();
                    let attr = |a: &str| decoded(p.attr(a));
                    if let Some((_, _, t)) = part.as_mut() {
                        t.start(name, &p);
                        continue;
                    }
                    if self.page_element(name, &p, &mut layout, &mut hf_style, &mut master) {
                        if let (Some((_, m)), Some(k)) = (master.as_mut(), hf_part(name)) {
                            m.parts[k] = HfPart {
                                present: true,
                                shown: p.attr("style:display") != "false",
                                text: None,
                            };
                            part = Some((k, depth, HfText::default()));
                        }
                        continue;
                    }
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
                    let family = p.attr("style:family");
                    match (name, family) {
                        ("style:style", "table-cell") => {
                            let opt = |a: &str| Some(attr(a)).filter(|s| !s.is_empty());
                            let name = attr("style:name");
                            self.cells.insert(
                                name.clone(),
                                CellStyle {
                                    data: opt("style:data-style-name"),
                                    parent: opt("style:parent-style-name"),
                                    props: CellProps::default(),
                                    maps: Vec::new(),
                                },
                            );
                            cell_style = Some(Some(name));
                            continue;
                        }
                        ("style:default-style", "table-cell") => {
                            cell_style = Some(None);
                            continue;
                        }
                        ("style:style", "table-column") => {
                            column_style = Some(Some(attr("style:name")));
                            continue;
                        }
                        ("style:default-style", "table-column") => {
                            column_style = Some(None);
                            continue;
                        }
                        ("style:style", "table-row") => {
                            row_style = Some(attr("style:name"));
                            continue;
                        }
                        ("style:style", "table") => {
                            let m = attr("style:master-page-name");
                            if !m.is_empty() {
                                self.table_masters.insert(attr("style:name"), m);
                            }
                            continue;
                        }
                        ("style:table-row-properties", _) => {
                            if let Some(n) = &row_style {
                                if p.attr("fo:break-before") == "page" {
                                    self.break_rows.insert(n.clone());
                                }
                            }
                            continue;
                        }
                        ("style:font-face", _) => {
                            let family = attr("svg:font-family");
                            if !family.is_empty() {
                                self.faces.insert(attr("style:name"), unquote(&family));
                            }
                            continue;
                        }
                        ("style:text-properties" | "style:table-cell-properties", _)
                            if cell_style.is_some() =>
                        {
                            let faces = std::mem::take(&mut self.faces);
                            let props = match cell_style.as_ref() {
                                Some(Some(n)) => self.cells.get_mut(n).map(|c| &mut c.props),
                                _ => Some(&mut self.default_cell),
                            };
                            if let Some(props) = props {
                                props.read(&p, &faces);
                            }
                            self.faces = faces;
                            continue;
                        }
                        ("style:map", _) if matches!(cell_style, Some(Some(_))) => {
                            if let Some(Some(n)) = &cell_style {
                                let map = (
                                    attr("style:condition"),
                                    attr("style:apply-style-name"),
                                    attr("style:base-cell-address"),
                                );
                                if let Some(c) = self.cells.get_mut(n) {
                                    c.maps.push(map);
                                }
                            }
                            continue;
                        }
                        ("style:table-column-properties", _) if column_style.is_some() => {
                            if let Some(Some(n)) = &column_style {
                                if p.attr("fo:break-before") == "page" {
                                    self.break_cols.insert(n.clone());
                                }
                            }
                            if let Some(pt) = length_pt(p.attr("style:column-width")) {
                                match column_style.clone().flatten() {
                                    Some(n) => {
                                        self.columns.insert(n, pt);
                                    }
                                    None => self.default_column = Some(pt),
                                }
                            }
                            continue;
                        }
                        _ => {}
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
                    if let Some((_, _, t)) = part.as_mut() {
                        t.text(p.text());
                    }
                    if text_into.is_some() {
                        XmlParser::append_decoded(p.text(), &mut text);
                    }
                }
                Event::End => {
                    let at = depth;
                    depth = depth.saturating_sub(1);
                    if let Some((k, d, mut t)) = part.take() {
                        if at != d {
                            t.end(p.name());
                            part = Some((k, d, t));
                            continue;
                        }
                        if let Some((_, m)) = master.as_mut() {
                            m.parts[k].text = t.finish();
                        }
                        continue;
                    }
                    match p.name() {
                        "style:page-layout" => {
                            if let Some((n, l)) = layout.take() {
                                self.layouts.insert(n, l);
                            }
                        }
                        "style:header-style" | "style:footer-style" => hf_style = None,
                        "style:master-page" => {
                            if let Some((n, m)) = master.take() {
                                self.masters.insert(n, m);
                            }
                        }
                        _ => {}
                    }
                    if matches!(p.name(), "style:style" | "style:default-style") {
                        cell_style = None;
                        column_style = None;
                        row_style = None;
                    }
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

    /// Read a page-layout or master-page element (#901) into `layout` or
    /// `master`. True when `name` was one: a header or footer part of the
    /// master page being read is the caller's to collect.
    fn page_element(
        &mut self,
        name: &str,
        p: &XmlParser,
        layout: &mut Option<(String, PageLayout)>,
        hf_style: &mut Option<bool>,
        master: &mut Option<(String, MasterPage)>,
    ) -> bool {
        let attr = |a: &str| decoded(p.attr(a));
        match name {
            "style:page-layout" => {
                *layout = Some((attr("style:name"), PageLayout::default()));
            }
            "style:page-layout-properties" => {
                if let Some((_, l)) = layout.as_mut() {
                    l.read(p);
                }
            }
            "style:header-style" => *hf_style = Some(false),
            "style:footer-style" => *hf_style = Some(true),
            "style:header-footer-properties" => {
                let (Some((_, l)), Some(footer)) = (layout.as_mut(), *hf_style) else {
                    return true;
                };
                let len = |a: &str| length_in(p.attr(a));
                let height = len("fo:min-height").or_else(|| len("svg:height"));
                // The spacing is the margin on the body's side.
                let spacing = len(if footer {
                    "fo:margin-top"
                } else {
                    "fo:margin-bottom"
                });
                let room = Some(height.unwrap_or(0.0) + spacing.unwrap_or(0.0));
                if footer {
                    l.footer_room = room;
                } else {
                    l.header_room = room;
                }
            }
            "style:master-page" => {
                *master = Some((
                    attr("style:name"),
                    MasterPage {
                        layout: attr("style:page-layout-name"),
                        ..MasterPage::default()
                    },
                ));
            }
            n if master.is_some() && hf_part(n).is_some() => {}
            _ => return false,
        }
        true
    }

    /// The page setup of a table in table style `style` (#901): its master
    /// page's (the one named `Default` when the style names none, or one
    /// that isn't there), its page layout and header and footer. With no
    /// master page, the model's default.
    fn page_setup(&self, style: &str) -> PageSetup {
        let mut ps = PageSetup::default();
        let Some(master) = self
            .table_masters
            .get(style)
            .and_then(|m| self.masters.get(m))
            .or_else(|| self.masters.get("Default"))
        else {
            return ps;
        };
        let [
            header,
            footer,
            header_left,
            footer_left,
            header_first,
            footer_first,
        ] = &master.parts;
        if let Some(l) = self.layouts.get(&master.layout) {
            let m = &mut ps.margins;
            if let Some(v) = l.margin_left {
                m.left = v;
            }
            if let Some(v) = l.margin_right {
                m.right = v;
            }
            // The page margin is the header's distance from the edge; the
            // body starts below the header and its spacing, when shown.
            let shown = |part: &HfPart| part.present && part.shown;
            if let Some(v) = l.margin_top {
                m.header = v;
                m.top = v + l.header_room.filter(|_| shown(header)).unwrap_or(0.0);
            }
            if let Some(v) = l.margin_bottom {
                m.footer = v;
                m.bottom = v + l.footer_room.filter(|_| shown(footer)).unwrap_or(0.0);
            }
            if let (Some(w), Some(h)) = (l.width, l.height) {
                if let Some(code) = crate::print::setup::paper_code(w, h) {
                    ps.paper_size = code;
                }
            }
            if let Some(o) = l.orientation {
                ps.orientation = o;
            }
            if let Some(n) = l.scale {
                ps.scale = n;
            }
            if l.fit_x.is_some() || l.fit_y.is_some() {
                ps.fit_to_page = true;
                ps.fit_width = l.fit_x.unwrap_or(0);
                ps.fit_height = l.fit_y.unwrap_or(0);
            }
            if l.over_then_down {
                ps.page_order = PageOrder::OverThenDown;
            }
            ps.first_page_number = l.first_page;
            (ps.h_centered, ps.v_centered) = l.centering;
            ps.grid_lines = l.grid;
            ps.headings = l.headings;
            if l.annotations {
                ps.cell_comments = CellComments::AtEnd;
            }
        }
        let hf = &mut ps.header_footer;
        hf.odd_header = header.shown_text();
        hf.odd_footer = footer.shown_text();
        // Even and first pages differ only where a shown part has text:
        // Excel's ODS writer gives every master page an empty first-page
        // header and footer, and an even one not displayed.
        hf.even_header = header_left.shown_text();
        hf.even_footer = footer_left.shown_text();
        hf.different_odd_even = hf.even_header.is_some() || hf.even_footer.is_some();
        hf.first_header = header_first.shown_text();
        hf.first_footer = footer_first.shown_text();
        hf.different_first = hf.first_header.is_some() || hf.first_footer.is_some();
        ps
    }

    /// The format code of cell style `name`, through its parents.
    fn code(&self, name: &str) -> Option<String> {
        let mut at = name;
        for _ in 0..16 {
            let style = self.cells.get(at)?;
            if let Some(d) = &style.data {
                return format_code(&self.data, d);
            }
            at = style.parent.as_deref()?;
        }
        None
    }

    /// The properties of cell style `name` (`None`: a cell with no style,
    /// which takes `Default`), through its parents and the default style.
    fn props(&self, name: Option<&str>) -> CellProps {
        let mut chain = Vec::new();
        let mut at = name.or(self.cells.contains_key("Default").then_some("Default"));
        while let Some(n) = at.filter(|_| chain.len() < 16) {
            let Some(style) = self.cells.get(n) else {
                break;
            };
            chain.push(&style.props);
            at = style.parent.as_deref();
        }
        chain
            .into_iter()
            .rev()
            .fold(self.default_cell.clone(), |acc, own| own.over(&acc))
    }

    /// The differential format of the style a conditional format applies:
    /// what it and its parents set, short of `Default` (which every cell
    /// has anyway). An explicit black is a colour it sets.
    fn dxf(&self, name: &str) -> crate::sheet::Dxf {
        let mut props = CellProps::default();
        let mut at = Some(name);
        for _ in 0..16 {
            let Some(style) = at
                .filter(|n| *n != "Default")
                .and_then(|n| self.cells.get(n))
            else {
                break;
            };
            props = props.over(&style.props);
            at = style.parent.as_deref();
        }
        crate::sheet::Dxf {
            fill: props.fill.flatten(),
            color: props.color.flatten(),
            bold: props.bold,
            italic: props.italic,
            ..crate::sheet::Dxf::default()
        }
    }

    /// The model format of cell style `name` (`None`: no style).
    fn xf(&self, name: Option<&str>) -> Xf {
        self.props(name).xf(self.code(name.unwrap_or("Default")))
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// Paragraph text being read (a cell's, an annotation's, a validation
/// message's): each `text:p` a line, `text:s` its spaces, `text:tab` a tab
/// and `text:line-break` a new line.
#[derive(Default)]
struct Para {
    text: String,
    paragraphs: usize,
}

impl Para {
    /// The text element `name` (one of the four above) starts.
    fn element(&mut self, name: &str, p: &XmlParser) {
        match name {
            "text:p" => {
                if self.paragraphs > 0 {
                    self.text.push('\n');
                }
                self.paragraphs += 1;
            }
            "text:s" => {
                let n = p.attr("text:c").parse().unwrap_or(1usize).min(1 << 12);
                self.text.push_str(&" ".repeat(n));
            }
            "text:tab" => self.text.push('\t'),
            _ => self.text.push('\n'),
        }
    }
}

/// The cell being read.
#[derive(Default)]
struct CellIn {
    attrs: HashMap<String, String>,
    text: Para,
    repeat: u32,
    /// Its annotation (#884): (author, text).
    comment: Option<(String, String)>,
}

/// An `office:annotation` being read: its author (`dc:creator`) and text.
#[derive(Default)]
struct NoteIn {
    author: String,
    text: Para,
}

/// The paragraph text the reader is in: a validation message's, else an
/// annotation's (directly inside it), else the cell's own (outside any
/// annotation or drawn shape).
fn active_para<'a>(
    message: &'a mut Option<(bool, Para)>,
    note: &'a mut Option<(usize, NoteIn)>,
    cell: &'a mut Option<CellIn>,
    hidden: usize,
) -> Option<&'a mut Para> {
    if let Some((_, m)) = message.as_mut() {
        return Some(m);
    }
    match hidden {
        0 => cell.as_mut().map(|c| &mut c.text),
        1 => note.as_mut().map(|(_, n)| &mut n.text),
        _ => None,
    }
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

// ---------------------------------------------------------------------------
// Conditions (data validation, conditional formatting)
// ---------------------------------------------------------------------------

/// `s` split at each top-level `sep` (outside quotes, brackets and
/// parentheses).
fn split_top<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    let (mut out, mut start, mut depth, mut quoted) = (Vec::new(), 0, 0i32, false);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => quoted = !quoted,
            b'(' | b'[' | b'{' if !quoted => depth += 1,
            b')' | b']' | b'}' if !quoted => depth -= 1,
            _ if !quoted && depth == 0 && s[i..].starts_with(sep) => {
                out.push(&s[start..i]);
                i += sep.len();
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

/// The arguments of `term` when it is a call of `name`, split at its
/// top-level `;`.
fn call_args<'a>(term: &'a str, name: &str) -> Option<Vec<&'a str>> {
    let inner = term.trim().strip_prefix(name)?.trim_start();
    let inner = inner.strip_prefix('(')?.strip_suffix(')')?;
    Some(split_top(inner, ";").into_iter().map(str::trim).collect())
}

/// A comparison's operator and the operand after it (`>=5` →
/// greaterThanOrEqual, `5`).
fn comparison(rest: &str) -> Option<(&'static str, &str)> {
    let rest = rest.trim_start();
    for (sym, op) in [
        ("<=", "lessThanOrEqual"),
        (">=", "greaterThanOrEqual"),
        ("!=", "notEqual"),
        ("<>", "notEqual"),
        ("=", "equal"),
        ("<", "lessThan"),
        (">", "greaterThan"),
    ] {
        if let Some(v) = rest.strip_prefix(sym) {
            return Some((op, v.trim()));
        }
    }
    None
}

/// An ODF expression operand in Excel's syntax.
fn operand(f: &str) -> Option<String> {
    convert_formula(f.trim())
}

/// A condition without its namespace prefix (`of:`, `ooow:`).
fn strip_ns(cond: &str) -> &str {
    match cond.split_once(':') {
        Some((ns, rest)) if !ns.is_empty() && ns.bytes().all(|b| b.is_ascii_alphabetic()) => rest,
        _ => cond,
    }
}

/// A validation's condition as the model holds it: its formulas in
/// Excel's syntax, relative to the rule's base cell.
#[derive(Clone, Debug, Default, PartialEq)]
struct Condition {
    /// `whole`, `decimal`, `list`, `custom`, ...; `""` for any value.
    kind: String,
    /// For the kinds that compare (`between`, `greaterThan`, ...); `""`
    /// for the others.
    operator: String,
    formula1: String,
    formula2: String,
}

/// A validation's `table:condition` (`of:cell-content-is-whole-number()
/// and cell-content-is-between(1;10)`, `cell-content-is-in-list("a";"b")`,
/// `is-true-formula([.A1]>0)`, ...). `None` for one this doesn't read.
fn validation_condition(cond: &str) -> Option<Condition> {
    let body = strip_ns(cond);
    // No condition: any value, the rule kept for its messages and flags
    // (as `.xls`/`.xlsb` keep a valType 0 rule).
    if body.trim().is_empty() {
        return Some(Default::default());
    }
    let (mut kind, mut op, mut f1, mut f2) = (String::new(), String::new(), None, None);
    for term in split_top(body, " and ") {
        let term = term.trim();
        let numeric = [
            ("cell-content-is-whole-number()", "whole"),
            ("cell-content-is-decimal-number()", "decimal"),
            ("cell-content-is-date()", "date"),
            ("cell-content-is-time()", "time"),
        ];
        if let Some((_, k)) = numeric.iter().find(|(t, _)| *t == term) {
            kind = k.to_string();
            continue;
        }
        let between = [
            ("cell-content-is-between", false, "between"),
            ("cell-content-is-not-between", false, "notBetween"),
            ("cell-content-text-length-is-between", true, "between"),
            (
                "cell-content-text-length-is-not-between",
                true,
                "notBetween",
            ),
        ];
        if let Some((args, text, o)) = between
            .iter()
            .find_map(|&(n, t, o)| Some((call_args(term, n)?, t, o)))
        {
            let [a, b] = args.as_slice() else {
                return None;
            };
            if text {
                kind = "textLength".to_string();
            }
            op = o.to_string();
            f1 = Some(operand(a)?);
            f2 = Some(operand(b)?);
            continue;
        }
        if let Some(rest) = term.strip_prefix("cell-content-text-length()") {
            let (o, v) = comparison(rest)?;
            kind = "textLength".to_string();
            op = o.to_string();
            f1 = Some(operand(v)?);
            continue;
        }
        if let Some(rest) = term.strip_prefix("cell-content()") {
            let (o, v) = comparison(rest)?;
            op = o.to_string();
            f1 = Some(operand(v)?);
            continue;
        }
        if let Some(args) = call_args(term, "cell-content-is-in-list") {
            kind = "list".to_string();
            let items: Option<Vec<String>> = args
                .iter()
                .map(|a| {
                    let inner = a.strip_prefix('"')?.strip_suffix('"')?;
                    Some(inner.replace("\"\"", "\""))
                })
                .collect();
            f1 = Some(match (items, args.as_slice()) {
                // Constants: Excel's one string of items.
                (Some(items), _) => format!("\"{}\"", items.join(",").replace('"', "\"\"")),
                (None, [one]) => operand(one)?,
                _ => return None,
            });
            continue;
        }
        if let Some(args) = call_args(term, "is-true-formula") {
            let [f] = args.as_slice() else {
                return None;
            };
            kind = "custom".to_string();
            f1 = Some(operand(f)?);
            continue;
        }
        return None;
    }
    if kind.is_empty() && f1.is_some() {
        kind = "decimal".to_string();
    }
    if crate::validation::takes_operator(&kind) && op.is_empty() {
        return None;
    }
    Some(Condition {
        kind,
        operator: op,
        formula1: f1.unwrap_or_default(),
        formula2: f2.unwrap_or_default(),
    })
}

/// A conditional format's condition, from a `style:map`
/// (`cell-content()>5`, `cell-content-is-between(1,5)`,
/// `is-true-formula([.A1]>0)`) or LibreOffice's `calcext:condition`
/// (`>5`, `between(1,5)`, `not-between(1,5)`, `formula-is([.A1]>0)`).
/// `None` for a kind the model doesn't evaluate (duplicates, top 10, text
/// tests, ...).
fn cf_condition(cond: &str) -> Option<crate::xlsx::NewCfRule> {
    use crate::xlsx::NewCfRule;
    let body = strip_ns(cond).trim();
    // Arguments split at `;`, or at `,` as LibreOffice writes two values.
    let args = |name: &str| -> Option<Vec<String>> {
        let a = call_args(body, name)?;
        let a = if a.len() == 1 {
            let inner = body
                .strip_prefix(name)?
                .trim()
                .strip_prefix('(')?
                .strip_suffix(')')?;
            split_top(inner, ",")
        } else {
            a
        };
        a.iter().map(|x| operand(x)).collect()
    };
    for (name, op) in [
        ("cell-content-is-between", "between"),
        ("cell-content-is-not-between", "notBetween"),
        ("between", "between"),
        ("not-between", "notBetween"),
    ] {
        if let Some(formulas) = args(name) {
            return (formulas.len() == 2).then(|| NewCfRule::CellIs {
                op: op.to_string(),
                formulas,
            });
        }
    }
    for name in ["is-true-formula", "formula-is"] {
        if let Some(a) = call_args(body, name) {
            let [f] = a.as_slice() else {
                return None;
            };
            return Some(NewCfRule::Expression {
                formula: operand(f)?,
            });
        }
    }
    let rest = body.strip_prefix("cell-content()").unwrap_or(body);
    let (op, v) = comparison(rest)?;
    Some(NewCfRule::CellIs {
        op: op.to_string(),
        formulas: vec![operand(v)?],
    })
}

/// `kind`'s formulas moved from `base` to `at` ([`super::rebase`]).
fn rebase_cf(
    mut kind: crate::xlsx::NewCfRule,
    base: Option<(u32, u32)>,
    at: (u32, u32),
) -> Option<crate::xlsx::NewCfRule> {
    for f in kind.formulas_mut() {
        *f = super::rebase(f, base, at)?;
    }
    Some(kind)
}

/// `s` split at each character `at` accepts outside a quoted sheet name
/// (`'Q1 Sales'`, `'A:B'`; `''` inside is a quote, which toggles twice),
/// empty pieces left out.
fn split_unquoted(s: &str, at: impl Fn(char) -> bool) -> Vec<&str> {
    let (mut out, mut start, mut quoted) = (Vec::new(), 0, false);
    for (i, ch) in s.char_indices() {
        if ch == '\'' {
            quoted = !quoted;
        } else if !quoted && at(ch) {
            out.push(&s[start..i]);
            start = i + ch.len_utf8();
        }
    }
    out.push(&s[start..]);
    out.retain(|p| !p.is_empty());
    out
}

/// An ODF range list (`S.A1:S.B2 S.D4`, `'Q1 Sales'.A1:'Q1 Sales'.A10`)
/// as (r1, c1, r2, c2) rectangles.
fn range_list(s: &str) -> Vec<(u32, u32, u32, u32)> {
    split_unquoted(s, char::is_whitespace)
        .into_iter()
        .filter_map(|r| {
            let (a, b) = match split_unquoted(r, |c| c == ':').as_slice() {
                [a] => (address_cell(a)?, address_cell(a)?),
                [a, b] => (address_cell(a)?, address_cell(b)?),
                _ => return None,
            };
            Some((a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)))
        })
        .collect()
}

/// An ODF cell address (`Sheet1.A1`, `$'My Sheet'.$B$2`) as (row, col).
fn address_cell(s: &str) -> Option<(u32, u32)> {
    let (_, cell) = split_address(s)?;
    crate::sheet::parse_cell_name(&cell)
}

/// A `table:content-validation` as read: its condition, base cell and
/// the rule's flags and texts (its ranges come from the cells naming it).
#[derive(Default)]
struct ValidationIn {
    condition: Option<Condition>,
    base: Option<(u32, u32)>,
    rule: crate::sheet::DataValidation,
}

/// Rectangles of cells, as row runs that grow down while the next row has
/// the same columns: a repeated row or column adds one range.
#[derive(Default)]
struct Ranges {
    rects: Vec<(u32, u32, u32, u32)>,
    /// The rectangle a run (c1, c2) ending at row r2 may extend.
    open: HashMap<(u32, u32), usize>,
}

impl Ranges {
    fn add(&mut self, r1: u32, r2: u32, c1: u32, c2: u32) {
        if let Some(&i) = self.open.get(&(c1, c2)) {
            if self.rects[i].2.checked_add(1) == Some(r1) {
                self.rects[i].2 = r2;
                return;
            }
        }
        self.rects.push((r1, c1, r2, c2));
        self.open.insert((c1, c2), self.rects.len() - 1);
    }
}

/// [`Ranges`] by name (a validation rule's, or a style with conditional
/// styles), in the order a table first names them.
#[derive(Default)]
struct KeyedRanges {
    order: Vec<(String, Ranges)>,
    at: HashMap<String, usize>,
}

impl KeyedRanges {
    fn add(&mut self, key: String, r1: u32, r2: u32, c1: u32, c2: u32) {
        let i = match self.at.get(&key) {
            Some(&i) => i,
            None => {
                self.at.insert(key.clone(), self.order.len());
                self.order.push((key, Ranges::default()));
                self.order.len() - 1
            }
        };
        self.order[i].1.add(r1, r2, c1, c2);
    }

    /// Every name's ranges, in order, leaving none.
    fn take(&mut self) -> Vec<(String, Ranges)> {
        self.at.clear();
        std::mem::take(&mut self.order)
    }
}

/// A table's column runs as read: (first column, count, width in points,
/// hidden).
type ColRun = (u32, u32, Option<f64>, bool);

/// The column definitions of a table's runs (#884). The width that covers
/// the most columns is the sheet's default and gives no definition (Excel
/// and LibreOffice give every column a width, the default ones too); it
/// is the sheet's default width unless it is Excel's own. Each other width
/// becomes one, as does a hidden column.
fn table_columns(runs: &[ColRun], sheet: &mut SheetIn) {
    use crate::print::paginate::width_of_px;
    let key = |pt: f64| (pt * 100.0).round() as i64;
    let mut cover: HashMap<Option<i64>, u64> = HashMap::new();
    for &(first, n, pt, _) in runs {
        let n = n.min(MAX_COLS.saturating_sub(first)) as u64;
        *cover.entry(pt.map(key)).or_default() += n;
    }
    // Most columns; a tie goes to the narrower width, so it is the same
    // on every run.
    let common = cover
        .into_iter()
        .max_by_key(|&(k, n)| (n, std::cmp::Reverse(k)))
        .and_then(|(k, _)| k);
    let chars = |pt: f64| width_of_px(pt * 96.0 / 72.0);
    if let Some(k) = common {
        let w = chars(k as f64 / 100.0);
        let dx = (w * 256.0).round();
        let excel = dx <= f64::from(u16::MAX)
            && super::xls::is_default_coldx(sheet.base_col_width, None, dx as u16);
        sheet.default_col_width = (!excel).then_some(w);
    }
    for &(first, n, pt, hidden) in runs {
        if first >= MAX_COLS || n == 0 {
            continue;
        }
        let width = pt.filter(|&pt| Some(key(pt)) != common).map(chars);
        if width.is_some() || hidden {
            sheet.cols.push(ColIn {
                first,
                last: first.saturating_add(n - 1).min(MAX_COLS - 1),
                width,
                hidden,
                ..ColIn::default()
            });
        }
    }
}

/// A cell's format in the book: its `book.xfs` index, and whether it
/// differs from the document's default format (a styled blank is kept
/// only then).
#[derive(Clone, Copy)]
struct Look {
    style: u32,
    styled: bool,
}

fn read_content(xml: &str, styles: &StyleSheet, limits: Limits) -> Result<BookIn, OpenError> {
    let mut book = BookIn::with_limits(limits);
    // Each cell style's format, made once (the key "" is no style).
    let mut looks: HashMap<String, Look> = HashMap::new();
    let doc_default = styles.xf(None);
    let mut look_of = |name: Option<&str>, book: &mut BookIn| -> Look {
        *looks
            .entry(name.unwrap_or("").to_string())
            .or_insert_with(|| {
                let xf = styles.xf(name);
                let styled = xf != doc_default;
                Look {
                    style: book.add_xf(xf),
                    styled,
                }
            })
    };
    let mut p = XmlParser::new(xml);
    let mut stack: Vec<String> = Vec::new();
    // Per table: the column default styles as runs (first column, style).
    let mut col_styles: Vec<(u32, String)> = Vec::new();
    // Whether any column default style gives an empty cell something to
    // keep (a format, or conditional styles), worked out once per table;
    // and what is left of the walk budget (`Limits::column_walk`).
    let mut col_interest: Option<bool> = None;
    let mut walk_left = limits.column_walk;
    let mut col_runs: Vec<ColRun> = Vec::new();
    let mut next_col = 0u32;
    let mut row = 0u32;
    let mut col = 0u32;
    let mut row_repeat = 1u32;
    let mut row_style = String::new();
    // The current row's cells, before its repeat is known to apply.
    let mut row_cells: Vec<(u32, Cell)> = Vec::new();
    // The current row's merges: (column, columns, rows) spanned.
    let mut row_merges: Vec<(u32, u32, u32)> = Vec::new();
    // The current row's comments: (column, author, text).
    let mut row_comments: Vec<(u32, String, String)> = Vec::new();
    // The annotation being read, and its depth in `stack`.
    let mut note: Option<(usize, NoteIn)> = None;
    // Data validation (#884): the rules by name, the one being read, the
    // message (help: false, error: true) whose paragraphs are being read,
    // the current row's runs (first, end, rule) and, per rule in the
    // order the table first names them, its cells.
    let mut validations: HashMap<String, ValidationIn> = HashMap::new();
    let mut validation: Option<(String, ValidationIn)> = None;
    let mut message: Option<(bool, Para)> = None;
    let mut row_rules: Vec<(u32, u32, String)> = Vec::new();
    let mut rule_cells = KeyedRanges::default();
    // Conditional formatting (#884): the current row's cells in a style
    // with `style:map`s (first, end, style) and, per such style in the
    // order the table first uses it, its cells; and the table's
    // LibreOffice conditional formats (`calcext:conditional-format`) and
    // the one being read.
    let mut row_maps: Vec<(u32, u32, String)> = Vec::new();
    let mut map_cells = KeyedRanges::default();
    let mut table_cf: Vec<super::CfIn> = Vec::new();
    let mut calc_cf: Option<super::CfIn> = None;
    let mut cell: Option<CellIn> = None;
    // Depth inside an annotation or a drawn shape, whose text is not the
    // cell's.
    let mut hidden = 0usize;
    // (name, sheet scope, definition) of each named range/expression.
    let mut names: Vec<(String, Option<usize>, String)> = Vec::new();
    let mut null_date_1904 = false;
    // Print titles (#901): where the table's header rows (columns) start,
    // and the span they cover once they end.
    let mut header_rows: Option<u32> = None;
    let mut header_cols: Option<u32> = None;
    let mut title_rows: Option<(u32, u32)> = None;
    let mut title_cols: Option<(u32, u32)> = None;

    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                let attr = |a: &str| decoded(p.attr(a));
                match name.as_str() {
                    "table:table" => {
                        let table = attr("table:name");
                        // Its print area (#901): the ranges of
                        // `table:print-ranges`, all of them or none.
                        let ranges = attr("table:print-ranges");
                        if let Some(area) = print_area(&table, &ranges) {
                            names.push((
                                crate::print::area::PRINT_AREA.to_string(),
                                Some(book.sheets.len()),
                                area,
                            ));
                        }
                        book.push_sheet(SheetIn {
                            name: table,
                            cells: BTreeMap::new(),
                            page_setup: styles.page_setup(&attr("table:style-name")),
                            ..SheetIn::default()
                        })?;
                        header_rows = None;
                        header_cols = None;
                        title_rows = None;
                        title_cols = None;
                        col_styles.clear();
                        col_interest = None;
                        col_runs.clear();
                        rule_cells.take();
                        map_cells.take();
                        table_cf.clear();
                        next_col = 0;
                        row = 0;
                    }
                    "table:table-column" => {
                        let n = p
                            .attr("table:number-columns-repeated")
                            .parse()
                            .unwrap_or(1u32);
                        if styles.break_cols.contains(&attr("table:style-name")) {
                            if let Some(sheet) = book.sheets.last_mut() {
                                add_breaks(&mut sheet.col_breaks, next_col, n, MAX_COLS, MAX_ROWS);
                            }
                        }
                        // Adjacent columns in one style are one run.
                        let style = attr("table:default-cell-style-name");
                        if col_styles.last().is_none_or(|(_, s)| *s != style) {
                            col_styles.push((next_col, style));
                        }
                        col_interest = None;
                        let width = styles
                            .columns
                            .get(&attr("table:style-name"))
                            .copied()
                            .or(styles.default_column);
                        let collapsed = p.attr("table:visibility") == "collapse";
                        col_runs.push((next_col, n, width, collapsed));
                        next_col = next_col.saturating_add(n);
                    }
                    "table:table-row" => {
                        col = 0;
                        row_cells.clear();
                        row_merges.clear();
                        row_comments.clear();
                        row_rules.clear();
                        row_maps.clear();
                        row_repeat = p
                            .attr("table:number-rows-repeated")
                            .parse()
                            .unwrap_or(1u32)
                            .max(1);
                        row_style = attr("table:default-cell-style-name");
                        if styles.break_rows.contains(&attr("table:style-name")) {
                            if let Some(sheet) = book.sheets.last_mut() {
                                add_breaks(
                                    &mut sheet.row_breaks,
                                    row,
                                    row_repeat,
                                    MAX_ROWS,
                                    MAX_COLS,
                                );
                            }
                        }
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
                    "office:annotation" if cell.is_some() && note.is_none() => {
                        hidden += 1;
                        note = Some((stack.len(), NoteIn::default()));
                    }
                    n if n == "office:annotation" || n.starts_with("draw:") => hidden += 1,
                    "text:p" | "text:s" | "text:tab" | "text:line-break" => {
                        if let Some(t) = active_para(&mut message, &mut note, &mut cell, hidden) {
                            t.element(&name, &p);
                        }
                    }
                    "table:content-validation" => {
                        let mut rule = crate::sheet::DataValidation {
                            allow_blank: p.attr("table:allow-empty-cell") != "false",
                            show_dropdown: p.attr("table:display-list") != "none",
                            ..crate::sheet::DataValidation::default()
                        };
                        let condition = validation_condition(&attr("table:condition"));
                        if let Some(c) = &condition {
                            rule.kind = c.kind.clone();
                            rule.operator = c.operator.clone();
                        }
                        validation = Some((
                            attr("table:name"),
                            ValidationIn {
                                condition,
                                base: address_cell(&attr("table:base-cell-address")),
                                rule,
                            },
                        ));
                    }
                    "calcext:conditional-format" => {
                        let ranges = range_list(&attr("calcext:target-range-address"));
                        // Each condition has a base of its own: its rules
                        // are moved to the block's anchor here.
                        calc_cf = Some(super::CfIn {
                            base: super::anchor_of(&ranges).unwrap_or_default(),
                            ranges,
                            rules: Vec::new(),
                        });
                    }
                    "calcext:condition" => {
                        if let Some(cf) = calc_cf.as_mut() {
                            let at = super::anchor_of(&cf.ranges);
                            let base = address_cell(&attr("calcext:base-cell-address"));
                            let rule = cf_condition(&attr("calcext:value"))
                                .zip(at)
                                .and_then(|(k, at)| rebase_cf(k, base, at));
                            if let Some(kind) = rule {
                                let dxf = styles.dxf(&attr("calcext:apply-style-name"));
                                cf.rules.push((kind, dxf));
                            }
                        }
                    }
                    "table:help-message" | "table:error-message" => {
                        if let Some((_, v)) = validation.as_mut() {
                            let error = name == "table:error-message";
                            let shown = p.attr("table:display") == "true";
                            let title = attr("table:title");
                            if error {
                                v.rule.show_error = shown;
                                v.rule.error_title = title;
                                v.rule.error_style = match p.attr("table:message-type") {
                                    "warning" => crate::sheet::AlertStyle::Warning,
                                    "information" => crate::sheet::AlertStyle::Information,
                                    _ => crate::sheet::AlertStyle::Stop,
                                };
                            } else {
                                v.rule.show_input = shown;
                                v.rule.prompt_title = title;
                            }
                            message = Some((error, Para::default()));
                        }
                    }
                    // The rows and columns repeated on every page (#901).
                    "table:table-header-rows" => header_rows = Some(row),
                    "table:table-header-columns" => header_cols = Some(next_col),
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
                // An annotation's author.
                if let Some((depth, n)) = note.as_mut().filter(|_| hidden == 1) {
                    if stack[*depth..].iter().any(|e| e == "dc:creator") {
                        XmlParser::append_decoded(p.text(), &mut n.author);
                    }
                }
                if stack.iter().any(|e| e == "text:p") {
                    if let Some(t) = active_para(&mut message, &mut note, &mut cell, hidden) {
                        XmlParser::append_decoded(p.text(), &mut t.text);
                    }
                }
            }
            Event::End => {
                let name = stack.pop().unwrap_or_default();
                match name.as_str() {
                    n if n == "office:annotation" || n.starts_with("draw:") => {
                        hidden = hidden.saturating_sub(1);
                        if note.as_ref().is_some_and(|(d, _)| *d == stack.len()) {
                            if let (Some((_, n)), Some(c)) = (note.take(), cell.as_mut()) {
                                c.comment = Some((n.author.trim().to_string(), n.text.text));
                            }
                        }
                    }
                    "table:table-header-rows" => {
                        title_rows = span(header_rows.take(), row, MAX_ROWS);
                    }
                    "table:table-header-columns" => {
                        title_cols = span(header_cols.take(), next_col, MAX_COLS);
                    }
                    "table:table" => {
                        // Its print titles (#901): columns first, then rows,
                        // as Excel writes them.
                        if let Some(sheet) = book.sheets.last() {
                            use crate::print::area::{PRINT_TITLES, PrintRef, spell_refs};
                            let refs: Vec<PrintRef> = [
                                title_cols.take().map(|(a, b)| PrintRef::Cols(a, b)),
                                title_rows.take().map(|(a, b)| PrintRef::Rows(a, b)),
                            ]
                            .into_iter()
                            .flatten()
                            .collect();
                            if !refs.is_empty() {
                                names.push((
                                    PRINT_TITLES.to_string(),
                                    Some(book.sheets.len() - 1),
                                    spell_refs(&super::escape_colon(&sheet.name), &refs),
                                ));
                            }
                        }
                        if let Some(sheet) = book.sheets.last_mut() {
                            table_columns(&col_runs, sheet);
                            for (name, cells) in rule_cells.take() {
                                let Some(v) = validations.get(&name) else {
                                    continue;
                                };
                                let Some(cond) = &v.condition else {
                                    continue;
                                };
                                let Some(anchor) = super::anchor_of(&cells.rects) else {
                                    continue;
                                };
                                // Based at the rule's base cell (none: its
                                // anchor); the build moves them.
                                sheet.validations.push(super::DvIn {
                                    rule: crate::sheet::DataValidation {
                                        ranges: cells.rects,
                                        formula1: cond.formula1.clone(),
                                        formula2: cond.formula2.clone(),
                                        ..v.rule.clone()
                                    },
                                    base: v.base.unwrap_or(anchor),
                                });
                            }
                            // LibreOffice writes its conditional formats
                            // twice: as calcext ones, and as `style:map`s
                            // for other readers. The calcext ones win.
                            if !table_cf.is_empty() {
                                sheet.cond_formats.append(&mut table_cf);
                            } else {
                                for (style, cells) in map_cells.take() {
                                    // Each map has a base of its own: its
                                    // rules are moved to the anchor here.
                                    let Some((r, c)) = super::anchor_of(&cells.rects) else {
                                        continue;
                                    };
                                    let maps =
                                        styles.cells.get(&style).map_or(&[][..], |s| &s.maps);
                                    let rules: Vec<_> = maps
                                        .iter()
                                        .filter_map(|(cond, apply, base)| {
                                            let kind = rebase_cf(
                                                cf_condition(cond)?,
                                                address_cell(base),
                                                (r, c),
                                            )?;
                                            Some((kind, styles.dxf(apply)))
                                        })
                                        .collect();
                                    if !rules.is_empty() {
                                        sheet.cond_formats.push(super::CfIn {
                                            ranges: cells.rects,
                                            rules,
                                            base: (r, c),
                                        });
                                    }
                                }
                            }
                        }
                    }
                    "calcext:conditional-format" => {
                        if let Some(cf) = calc_cf.take() {
                            if !cf.ranges.is_empty() && !cf.rules.is_empty() {
                                table_cf.push(cf);
                            }
                        }
                    }
                    "table:content-validation" => {
                        if let Some((name, v)) = validation.take() {
                            validations.insert(name, v);
                        }
                    }
                    "table:help-message" | "table:error-message" => {
                        if let (Some((error, Para { text, .. })), Some((_, v))) =
                            (message.take(), validation.as_mut())
                        {
                            if error {
                                v.rule.error = text;
                            } else {
                                v.rule.prompt = (!text.is_empty()).then_some(text);
                            }
                        }
                    }
                    "table:table-cell" | "table:covered-table-cell" => {
                        let Some(c) = cell.take() else { continue };
                        let repeat = c.repeat;
                        let last = col.saturating_add(repeat).min(MAX_COLS);
                        let own = c
                            .attrs
                            .get("table:style-name")
                            .filter(|s| !s.is_empty())
                            .cloned()
                            .or_else(|| (!row_style.is_empty()).then(|| row_style.clone()));
                        // The column default style of each column from
                        // `from`: runs of (first, end, style).
                        let by_column = |from: u32, to: u32| -> Vec<(u32, u32, Option<String>)> {
                            let mut out = Vec::new();
                            let mut at = from;
                            while at < to {
                                let k = col_styles.partition_point(|(first, _)| *first <= at);
                                let style = k
                                    .checked_sub(1)
                                    .map(|k| col_styles[k].1.clone())
                                    .filter(|s| !s.is_empty());
                                let end =
                                    col_styles.get(k).map_or(to, |(first, _)| (*first).min(to));
                                out.push((at, end, style));
                                at = end;
                            }
                            out
                        };
                        // The run's styles, column by column where it has
                        // none of its own, unless it reaches the grid's
                        // last column (padding): its styled blanks and
                        // conditional styles.
                        // A run with no style of its own walks the column
                        // styles it crosses only when one of them matters,
                        // and only while the walk budget lasts: the walk
                        // is bounded however many rows and columns.
                        let runs = if name == "table:table-cell" && last < MAX_COLS {
                            match &own {
                                Some(s) => vec![(col, last, Some(s.clone()))],
                                None => {
                                    let interest = *col_interest.get_or_insert_with(|| {
                                        col_styles.iter().any(|(_, s)| {
                                            !s.is_empty()
                                                && (look_of(Some(s), &mut book).styled
                                                    || styles
                                                        .cells
                                                        .get(s)
                                                        .is_some_and(|x| !x.maps.is_empty()))
                                        })
                                    });
                                    if interest && walk_left > 0 {
                                        let runs = by_column(col, last);
                                        walk_left = walk_left.saturating_sub(runs.len());
                                        runs
                                    } else {
                                        Vec::new()
                                    }
                                }
                            }
                        } else {
                            Vec::new()
                        };
                        if let Some(value) = cell_value(&c, null_date_1904) {
                            let style_name = own.clone().or_else(|| {
                                by_column(col, col.saturating_add(1))
                                    .pop()
                                    .and_then(|r| r.2)
                            });
                            let style = look_of(style_name.as_deref(), &mut book).style;
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
                            let n = last.saturating_sub(col) as usize;
                            let copies = n.saturating_sub(1);
                            book.charge_repeats(copies, copies.saturating_mul(copy_bytes(&made)))?;
                            book.charge_cells(n)?;
                            for cc in col..last {
                                row_cells.push((cc, made.clone()));
                            }
                        } else {
                            // A styled blank (#884).
                            for (from, to, style) in &runs {
                                let look = look_of(style.as_deref(), &mut book);
                                if look.styled && book.take_styled_blanks((to - from) as usize) {
                                    let made = Cell {
                                        style: look.style,
                                        ..Cell::default()
                                    };
                                    row_cells.extend((*from..*to).map(|cc| (cc, made.clone())));
                                }
                            }
                        }
                        // A merged range starts here (its first copy only).
                        let span = |a: &str| {
                            c.attrs
                                .get(a)
                                .and_then(|v| v.parse::<u32>().ok())
                                .unwrap_or(1)
                                .max(1)
                        };
                        let (cs, rs) = (
                            span("table:number-columns-spanned"),
                            span("table:number-rows-spanned"),
                        );
                        if (cs > 1 || rs > 1) && col < MAX_COLS {
                            row_merges.push((col, cs, rs));
                        }
                        // The cells of a style with conditional styles.
                        for (from, to, style) in runs {
                            if let Some(st) = style.filter(|st| {
                                styles.cells.get(st).is_some_and(|x| !x.maps.is_empty())
                            }) {
                                row_maps.push((from, to, st));
                            }
                        }
                        if let Some(rule) = c
                            .attrs
                            .get("table:content-validation-name")
                            .filter(|n| !n.is_empty() && col < MAX_COLS)
                        {
                            row_rules.push((col, last, rule.clone()));
                        }
                        // A repeated cell's comment is on its first copy
                        // only, as is a repeated row's: a copy of a note
                        // is not another note.
                        if let Some((author, text)) = c.comment.filter(|_| col < MAX_COLS) {
                            row_comments.push((col, author, text));
                        }
                        col = col.saturating_add(repeat);
                    }
                    "table:table-row" => {
                        let last = row.saturating_add(row_repeat).min(MAX_ROWS);
                        // A run of rows to the grid's last row is padding:
                        // its styled blanks and merges are not kept.
                        let padding = row_repeat > 1 && last == MAX_ROWS;
                        if padding {
                            row_cells.retain(|(_, c)| c.value != CellValue::Empty);
                            row_merges.clear();
                        }
                        if (!row_cells.is_empty() || !row_merges.is_empty())
                            && !book.sheets.is_empty()
                        {
                            // Values and styled blanks apart, once: a copy
                            // takes its blanks only while they fit the cap,
                            // and a row of nothing else stops copying when
                            // they don't. So the time is bounded by the
                            // values and merges charged and the cap, never
                            // rows x blanks.
                            let (blanks, values): (Vec<_>, Vec<_>) = std::mem::take(&mut row_cells)
                                .into_iter()
                                .partition(|(_, c)| c.value == CellValue::Empty);
                            // The first instance was charged cell by cell;
                            // each further row copies them all.
                            let extra = (last.saturating_sub(row) as usize).saturating_sub(1);
                            let row_bytes: usize = values.iter().map(|(_, c)| copy_bytes(c)).sum();
                            let copies = extra.saturating_mul(values.len());
                            book.charge_repeats(copies, extra.saturating_mul(row_bytes))?;
                            book.charge_cells(copies)?;
                            let merge_copies = extra.saturating_mul(row_merges.len());
                            book.charge_repeats(merge_copies, 0)?;
                            for r in row..last {
                                let with_blanks = !blanks.is_empty()
                                    && (r == row || book.take_styled_blanks(blanks.len()));
                                if !with_blanks && values.is_empty() && row_merges.is_empty() {
                                    break;
                                }
                                let Some(sheet) = book.sheets.last_mut() else {
                                    break;
                                };
                                if with_blanks {
                                    for (cc, made) in &blanks {
                                        sheet.cells.insert((r, *cc), made.clone());
                                    }
                                }
                                for (cc, made) in &values {
                                    sheet.cells.insert((r, *cc), made.clone());
                                }
                                for &(c, cs, rs) in &row_merges {
                                    let r2 = r.saturating_add(rs - 1).min(MAX_ROWS - 1);
                                    let c2 = c.saturating_add(cs - 1).min(MAX_COLS - 1);
                                    sheet.merges.push((r, c, r2, c2));
                                }
                            }
                        }
                        // A style's cells with conditional styles, but not
                        // in padding.
                        if row < MAX_ROWS && !padding {
                            for (c1, c2, style) in row_maps.drain(..) {
                                map_cells.add(style, row, last - 1, c1, c2 - 1);
                            }
                        }
                        // The cells naming a validation rule: one range per
                        // run, however many rows repeat it.
                        if row < MAX_ROWS {
                            for (c1, c2, rule) in row_rules.drain(..) {
                                rule_cells.add(rule, row, last - 1, c1, c2 - 1);
                            }
                        }
                        if let Some(sheet) = book.sheets.last_mut().filter(|_| row < MAX_ROWS) {
                            for (c, author, text) in row_comments.drain(..) {
                                sheet.comments.push((row, c, author, text));
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

/// Manual page breaks before each of `n` rows (columns) from `first` (#901):
/// row (column) 0 has nothing before it, none past the grid's `max`, and at
/// most [`super::page::MAX_BREAKS`] in all. Each spans the whole sheet, to
/// `span` - 1, as Excel writes them. Bounded by the cap, never by `n`, so a
/// filler run of a million rows costs nothing.
fn add_breaks(list: &mut Vec<crate::sheet::PageBreak>, first: u32, n: u32, max: u32, span: u32) {
    let end = first.saturating_add(n).min(max);
    for at in first.max(1)..end {
        if list.len() >= super::page::MAX_BREAKS {
            break;
        }
        list.push(super::page::read_break(at, 0, span - 1));
    }
}

/// The rows (columns) from `start` up to `end`, on the grid of `max`.
fn span(start: Option<u32>, end: u32, max: u32) -> Option<(u32, u32)> {
    let start = start?;
    let end = end.min(max);
    (end > start).then(|| (start, end - 1))
}

/// A table's `table:print-ranges` (space-separated ODF ranges) as Excel's
/// `_xlnm.Print_Area` definition on table `table` (#901); `None` when it
/// has none, or a range that doesn't read.
fn print_area(table: &str, ranges: &str) -> Option<String> {
    use crate::print::area::{PrintRef, parse_refs, spell_refs};
    let mut refs = Vec::new();
    for r in split_unquoted(ranges, char::is_whitespace) {
        let excel = convert_ref(r)?;
        let [one] = parse_refs(&excel)[..] else {
            return None;
        };
        refs.push(PrintRef::of_rect(one.rect()));
    }
    // The table's name with `:` spelled as the build's marker, so a
    // renamed sheet's definition follows it (as `sheet_prefix` spells).
    (!refs.is_empty()).then(|| spell_refs(&super::escape_colon(table), &refs))
}

/// A cell's value from its `office:value-type` and value attributes (or its
/// text); `None` for an empty cell, which then takes no place in the model.
fn cell_value(c: &CellIn, date1904: bool) -> Option<CellValue> {
    let a = |k: &str| c.attrs.get(k).map(String::as_str);
    let text = || a("office:string-value").map_or_else(|| c.text.text.clone(), str::to_string);
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
        _ if !c.text.text.is_empty() => CellValue::Text(c.text.text.clone()),
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
        // An external reference is dropped (the cell keeps its value), not
        // stored as text no reader parses.
        assert_eq!(c("of:=['file:///C:/b.ods'#$Sheet1.A1]"), None);
        assert_eq!(c("of:=SUM(['file:///C:/b.ods'#$Sheet1.A1:.B2])"), None);
        assert_eq!(c("of:=[$'My Sheet'.B2]+[file:///b.ods#$Sheet1.A1]"), None);
        assert!(a1_shaped("$A$1") && a1_shaped("XFD") && a1_shaped("$3") && a1_shaped("A1"));
        assert!(!a1_shaped("") && !a1_shaped("$") && !a1_shaped("file") && !a1_shaped("#A1"));
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
            <table:table table:name="Calc">{}{}{}{}{}</table:table>
            <table:named-expressions><table:named-range table:name="Col" table:cell-range-address="$'A:B'.$A$1:.$A$3"/></table:named-expressions>"#,
            num(1),
            num(2),
            num(3),
            num(10),
            f("of:=SUM(['A:B'.A:.A])"),
            f("of:=['A:B'.A2]"),
            f("of:=SUM(['A:B'.A1:'C'.A1])"),
            f("of:=['X:Y'.A1]"),
            f("of:=['A:B'.A1]+['X:Y'.A1]"),
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
        assert_eq!(got(4).0, "A_B!A1+'X:Y'!A1");
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

    // --- #884: cell formats, styled blanks, column widths and merges ----

    /// A content.xml with these font faces, automatic styles and tables.
    fn styled_content(faces: &str, auto: &str, body: &str) -> String {
        format!(
            r#"<office:document-content xmlns:office="o" xmlns:table="t" xmlns:text="x" xmlns:number="n" xmlns:style="s" xmlns:fo="f" xmlns:svg="g" xmlns:dc="d"><office:font-face-decls>{faces}</office:font-face-decls><office:automatic-styles>{auto}</office:automatic-styles><office:body><office:spreadsheet>{body}</office:spreadsheet></office:body></office:document-content>"#
        )
    }

    /// Read `content` after the styles part `styles_xml` (may be empty),
    /// under `limits`.
    fn read_styled(styles_xml: &str, content: &str, limits: Limits) -> BookIn {
        let mut styles = StyleSheet::default();
        styles.read(styles_xml);
        styles.read(content);
        read_content(content, &styles, limits).unwrap()
    }

    fn xf_at(b: &BookIn, r: u32, c: u32) -> Xf {
        b.xfs[b.sheets[0].cells[&(r, c)].style as usize].clone()
    }

    /// A cell style's text and cell properties become the cell's model
    /// format, through its parent; a thick or partial border is no box.
    #[test]
    fn ods_cell_style_formatting() {
        let xml = styled_content(
            r#"<style:font-face style:name="Arial1" svg:font-family="'Arial'"/>"#,
            r##"<style:style style:name="ce1" style:family="table-cell">
                 <style:table-cell-properties fo:background-color="#ffff00" fo:border="0.06pt solid #000000"/>
                 <style:text-properties fo:font-weight="bold" fo:font-style="italic" fo:color="#ff0000" fo:font-size="14pt" style:font-name="Arial1"/></style:style>
               <style:style style:name="ce2" style:family="table-cell" style:parent-style-name="ce1">
                 <style:table-cell-properties fo:background-color="transparent" fo:border="none" fo:border-top="0.74pt solid #ff0000" fo:border-bottom="0.74pt solid #000000" fo:border-left="0.74pt solid #000000" fo:border-right="0.74pt solid #000000"/>
                 <style:text-properties fo:font-weight="normal" fo:color="#000000"/></style:style>
               <style:style style:name="ce3" style:family="table-cell">
                 <style:table-cell-properties fo:border="2.5pt solid #000000"/></style:style>
               <style:style style:name="ce4" style:family="table-cell">
                 <style:table-cell-properties fo:border-top="0.74pt solid #000000" fo:border-bottom="0.74pt solid #000000"/></style:style>"##,
            r#"<table:table table:name="S"><table:table-row>
                 <table:table-cell table:style-name="ce1" office:value-type="float" office:value="1"/>
                 <table:table-cell table:style-name="ce2" office:value-type="float" office:value="2"/>
                 <table:table-cell table:style-name="ce3" office:value-type="float" office:value="3"/>
                 <table:table-cell table:style-name="ce4" office:value-type="float" office:value="4"/>
                 <table:table-cell office:value-type="float" office:value="5"/>
               </table:table-row></table:table>"#,
        );
        let b = read_styled("", &xml, Limits::default());
        let x = xf_at(&b, 0, 0);
        assert!(x.bold && x.italic && x.border, "{x:?}");
        assert_eq!(x.color, Some((255, 0, 0)));
        assert_eq!(x.fill, Some((255, 255, 0)));
        assert_eq!(x.font_size, Some(14.0));
        assert_eq!(x.font_name.as_deref(), Some("Arial"));
        // The four sides set one by one: a box, in any colour; the rest
        // as ce2 overrides its parent.
        let x = xf_at(&b, 0, 1);
        assert!(x.border && !x.bold && x.italic, "{x:?}");
        assert_eq!((x.color, x.fill, x.font_size), (None, None, Some(14.0)));
        // A thick box and two sides are no thin box.
        assert!(!xf_at(&b, 0, 2).border);
        assert!(!xf_at(&b, 0, 3).border);
        assert_eq!(b.sheets[0].cells[&(0, 4)].style, 0);
    }

    /// An empty cell whose style differs from the document's default is
    /// kept for its format; one in the default style isn't.
    #[test]
    fn ods_styled_blank_kept() {
        let xml = styled_content(
            "",
            r##"<style:style style:name="ce1" style:family="table-cell"><style:table-cell-properties fo:background-color="#00ff00"/></style:style>
               <style:style style:name="ce2" style:family="table-cell"/>"##,
            r#"<table:table table:name="S">
                 <table:table-column table:number-columns-repeated="2"/>
                 <table:table-column table:default-cell-style-name="ce1" table:number-columns-repeated="2"/>
                 <table:table-column table:number-columns-repeated="16380"/>
                 <table:table-row>
                   <table:table-cell table:style-name="ce1" table:number-columns-repeated="2"/>
                   <table:table-cell table:style-name="ce2"/>
                   <table:table-cell/>
                   <table:table-cell table:number-columns-repeated="16380"/>
                 </table:table-row>
                 <table:table-row table:number-rows-repeated="2">
                   <table:table-cell table:number-columns-repeated="4"/>
                 </table:table-row>
               </table:table>"#,
        );
        let b = read_styled("", &xml, Limits::default());
        let cells = &b.sheets[0].cells;
        let at: Vec<_> = cells.keys().copied().collect();
        // A1:B1 by their own style, D1 by its column's; C3:D3 and C2:D2
        // (the repeated row) by their columns'. The rest is the default.
        assert_eq!(at, [(0, 0), (0, 1), (0, 3), (1, 2), (1, 3), (2, 2), (2, 3)]);
        for c in cells.values() {
            assert_eq!(c.value, CellValue::Empty);
            assert_eq!(b.xfs[c.style as usize].fill, Some((0, 255, 0)));
        }
    }

    /// A LibreOffice document: its default cell style is Liberation Sans
    /// 10, every column defaults to it, and padding fills the grid. No
    /// empty cell is kept, and a value takes the default's font, as the
    /// `.xls` reader keeps a workbook's Arial 10 (and Excel's `.xlsx` of
    /// an `.ods` whose default is Aptos Narrow has it on every cell).
    #[test]
    fn ods_libreoffice_default_style_is_no_format() {
        let styles_xml = r#"<office:document-styles xmlns:office="o" xmlns:style="s" xmlns:fo="f" xmlns:svg="g"><office:font-face-decls><style:font-face style:name="Liberation Sans" svg:font-family="'Liberation Sans'"/></office:font-face-decls><office:styles>
            <style:default-style style:family="table-cell"><style:text-properties style:font-name="Liberation Sans" fo:font-size="10pt"/></style:default-style>
            <style:default-style style:family="table-column"><style:table-column-properties style:column-width="0.889in"/></style:default-style>
            <style:style style:name="Default" style:family="table-cell"/>
            </office:styles></office:document-styles>"#;
        let xml = styled_content(
            "",
            r#"<style:style style:name="co1" style:family="table-column"><style:table-column-properties style:column-width="0.889in"/></style:style>
               <style:style style:name="ce1" style:family="table-cell" style:parent-style-name="Default"/>"#,
            r#"<table:table table:name="S">
                 <table:table-column table:style-name="co1" table:number-columns-repeated="1024" table:default-cell-style-name="Default"/>
                 <table:table-row><table:table-cell table:style-name="ce1" office:value-type="float" office:value="1"/><table:table-cell table:number-columns-repeated="1023"/></table:table-row>
                 <table:table-row table:number-rows-repeated="1048575"><table:table-cell table:number-columns-repeated="1024"/></table:table-row>
               </table:table>"#,
        );
        let b = read_styled(styles_xml, &xml, Limits::default());
        let s = &b.sheets[0];
        assert_eq!(s.cells.len(), 1);
        let x = xf_at(&b, 0, 0);
        assert_eq!(x.font_name.as_deref(), Some("Liberation Sans"));
        assert_eq!(x.font_size, Some(10.0));
        assert!(!x.bold && x.fill.is_none() && !x.border);
        // Every column is at the default width: no definition, and the
        // sheet's default is LibreOffice's 0.889in (85 px).
        assert!(s.cols.is_empty());
        let w = s.default_col_width.unwrap();
        assert_eq!(crate::print::paginate::width_px(w), 85.0);
    }

    /// The width most columns have is the default; the others are column
    /// definitions in Excel's units, as is a collapsed column. A spanned
    /// cell is a merge, made in each copy of a repeated row.
    #[test]
    fn ods_column_widths_and_spans() {
        let xml = styled_content(
            "",
            r#"<style:style style:name="co1" style:family="table-column"><style:table-column-properties style:column-width="1.5875cm"/></style:style>
               <style:style style:name="co2" style:family="table-column"><style:table-column-properties style:column-width="1.5in"/></style:style>"#,
            r#"<table:table table:name="S">
                 <table:table-column table:style-name="co1"/>
                 <table:table-column table:style-name="co2" table:number-columns-repeated="2"/>
                 <table:table-column table:style-name="co1" table:visibility="collapse"/>
                 <table:table-column table:style-name="co1" table:number-columns-repeated="16380"/>
                 <table:table-row>
                   <table:table-cell table:number-columns-spanned="2" table:number-rows-spanned="3" office:value-type="float" office:value="1"/>
                   <table:covered-table-cell/>
                 </table:table-row>
                 <table:table-row table:number-rows-repeated="2"><table:covered-table-cell/>
                   <table:table-cell table:number-columns-spanned="3"/>
                 </table:table-row>
                 <table:table-row table:number-rows-repeated="1048573">
                   <table:table-cell table:number-columns-spanned="2"/>
                 </table:table-row>
               </table:table>"#,
        );
        let b = read_styled("", &xml, Limits::default());
        let s = &b.sheets[0];
        assert_eq!(s.default_col_width, None);
        let cols: Vec<_> = s
            .cols
            .iter()
            .map(|c| (c.first, c.last, c.width, c.hidden))
            .collect();
        // 1.5in = 144 px, which Excel stores as 144/7 characters.
        let w = (144.0f64 / 7.0 * 256.0).trunc() / 256.0;
        assert_eq!(cols, [(1, 2, Some(w), false), (3, 3, None, true)]);
        assert_eq!(crate::print::paginate::width_px(w), 144.0);
        // The padding row's span is not a merge.
        assert_eq!(s.merges, [(0, 0, 2, 1), (1, 1, 1, 3), (2, 1, 2, 3)]);
    }

    /// Styled blanks never refuse a file: padding in a style to the grid's
    /// edge costs nothing, the rest has its own cap past which it is left
    /// out, and none of it counts against the repeat budget.
    #[test]
    fn ods_padding_of_styled_blanks_opens() {
        let auto = r##"<style:style style:name="ce1" style:family="table-cell"><style:table-cell-properties fo:background-color="#00ff00"/></style:style>"##;
        // Padding in a style, as wide and as deep as the grid.
        let xml = styled_content(
            "",
            auto,
            r#"<table:table table:name="S"><table:table-column table:number-columns-repeated="16384" table:default-cell-style-name="ce1"/>
                 <table:table-row><table:table-cell office:value-type="float" office:value="1"/><table:table-cell table:style-name="ce1" table:number-columns-repeated="16383"/></table:table-row>
                 <table:table-row table:number-rows-repeated="1048575"><table:table-cell table:number-columns-repeated="16384"/></table:table-row>
               </table:table>"#,
        );
        let started = std::time::Instant::now();
        let b = read_styled("", &xml, Limits::default());
        assert_eq!(b.sheets[0].cells.len(), 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));

        // A styled 3x2000 block is kept, whole; at a repeat budget the
        // values use up, it still opens.
        let block = |rows: u32| {
            styled_content(
                "",
                auto,
                &format!(
                    r#"<table:table table:name="S">
                     <table:table-row table:number-rows-repeated="4"><table:table-cell office:value-type="float" office:value="1"/></table:table-row>
                     <table:table-row table:number-rows-repeated="{rows}"><table:table-cell table:style-name="ce1" table:number-columns-repeated="3"/></table:table-row>
                   </table:table>"#
                ),
            )
        };
        let at_budget = Limits {
            repeat_cells: 3,
            ..Limits::default()
        };
        let b = read_styled("", &block(2000), at_budget);
        assert_eq!(b.sheets[0].cells.len(), 4 + 3 * 2000);

        // Old LibreOffice pads rows to 1024 columns, not the grid's
        // 16384: past the cap, the blanks are left out, and the file opens.
        let old = styled_content(
            "",
            auto,
            r#"<table:table table:name="S">
                 <table:table-row table:number-rows-repeated="5000"><table:table-cell table:style-name="ce1" table:number-columns-repeated="1024"/></table:table-row>
                 <table:table-row><table:table-cell office:value-type="float" office:value="2"/></table:table-row>
               </table:table>"#,
        );
        let capped = Limits {
            styled_blanks: 10_000,
            ..Limits::default()
        };
        let b = read_styled("", &old, capped);
        let cells = &b.sheets[0].cells;
        // Nine rows of 1024 fit; the value after them is still there.
        assert_eq!(cells.len(), 9 * 1024 + 1);
        assert_eq!(cells[&(5000, 0)].value, CellValue::Number(2.0));
    }

    /// The content.xml of [`round_trip_ods`].
    fn feature_ods_content() -> String {
        styled_content(
            r#"<style:font-face style:name="Arial" svg:font-family="Arial"/>"#,
            r##"<style:style style:name="ce1" style:family="table-cell">
                 <style:table-cell-properties fo:background-color="#ffff00" fo:border="0.74pt solid #000000"/>
                 <style:text-properties fo:font-weight="bold" fo:font-style="italic" fo:color="#ff0000" fo:font-size="14pt" style:font-name="Arial"/></style:style>
               <style:style style:name="co1" style:family="table-column"><style:table-column-properties style:column-width="1.5875cm"/></style:style>
               <style:style style:name="co2" style:family="table-column"><style:table-column-properties style:column-width="1.5in"/></style:style>
               <style:style style:name="cf1" style:family="table-cell"><style:table-cell-properties fo:background-color="#00ff00"/></style:style>
               <style:style style:name="ce2" style:family="table-cell"><style:map style:condition="cell-content()&gt;5" style:apply-style-name="cf1" style:base-cell-address="S.B4"/></style:style>"##,
            r#"<table:content-validations><table:content-validation table:name="v1" table:condition="of:is-true-formula([.A1]&gt;0)" table:base-cell-address="S.A1"/></table:content-validations>
               <table:table table:name="S">
                 <table:table-column table:style-name="co1"/>
                 <table:table-column table:style-name="co2"/>
                 <table:table-column table:style-name="co1" table:number-columns-repeated="16382"/>
                 <table:table-row><table:table-cell table:style-name="ce1" office:value-type="float" office:value="1"/><table:table-cell table:content-validation-name="v1" table:number-columns-repeated="2"/></table:table-row>
                 <table:table-row><table:table-cell><office:annotation><dc:creator>Ann</dc:creator><text:p>a note</text:p></office:annotation></table:table-cell></table:table-row>
                 <table:table-row><table:table-cell table:number-columns-spanned="2" table:number-rows-spanned="2"/></table:table-row>
                 <table:table-row><table:table-cell/><table:table-cell table:style-name="ce2" office:value-type="float" office:value="7"/><table:table-cell table:style-name="ce2" office:value-type="float" office:value="3"/></table:table-row>
               </table:table>"#,
        )
    }

    fn ods_zip(content: &str) -> Vec<u8> {
        opccore::zipwrite::write_zip(&[
            (
                "mimetype".to_string(),
                b"application/vnd.oasis.opendocument.spreadsheet".to_vec(),
            ),
            ("content.xml".to_string(), content.as_bytes().to_vec()),
        ])
    }

    /// Opened, saved as `.xlsx` and reloaded, an `.ods` keeps what the
    /// import read (#884: they were lost on save).
    #[test]
    fn round_trip_ods() {
        let (pkg, _) = super::super::open_workbook(&ods_zip(&feature_ods_content())).unwrap();
        let back = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        let wb = &back.workbook;
        let s = &wb.sheets[0];
        let style = s.cell(0, 0).unwrap().style;
        let x = wb.styles.xf(style);
        assert!(x.bold && x.italic, "{x:?}");
        assert!(super::super::saved_thin_box(&back, style));
        assert_eq!(x.color, Some((255, 0, 0)));
        assert_eq!(x.fill, Some((255, 255, 0)));
        assert_eq!(x.font_size, Some(14.0));
        assert_eq!(x.font_name.as_deref(), Some("Arial"));
        let widths: Vec<_> = s.col_defs.iter().map(|d| (d.min, d.max, d.width)).collect();
        let w = (144.0f64 / 7.0 * 256.0).trunc() / 256.0;
        assert_eq!(widths, [(1, 1, Some(w))]);
        assert_eq!(s.merges, [(2, 0, 3, 1)]);
        let notes: Vec<_> = back
            .comments()
            .into_iter()
            .map(|c| (c.row, c.col, c.author, c.text))
            .collect();
        assert_eq!(notes, [(1, 0, "Ann".to_string(), "a note".to_string())]);
        let v = &s.validations;
        assert_eq!(v.len(), 1);
        // Based at A1, `[.A1]` is the cell itself: B1 at the range's top.
        assert_eq!(
            (v[0].kind.as_str(), v[0].formula1.as_str()),
            ("custom", "B1>0")
        );
        assert_eq!(v[0].ranges, [(0, 1, 0, 2)]);
        // B4's 7 is on green; C4's 3 isn't.
        assert_eq!(s.cond_formats.len(), 1);
        assert_eq!(
            crate::cf::cell_dxf(wb, 0, 3, 1).unwrap().fill,
            Some((0, 255, 0))
        );
        assert!(crate::cf::cell_dxf(wb, 0, 3, 2).is_none());
    }

    /// An annotation is its cell's comment: `dc:creator` the author, each
    /// paragraph a line; the cell's own text is not touched. A repeated
    /// cell's or row's annotation is on its first copy only.
    #[test]
    fn ods_annotations_become_comments() {
        let xml = styled_content(
            "",
            "",
            r#"<table:table table:name="S">
                 <table:table-row>
                   <table:table-cell office:value-type="string"><office:annotation><dc:creator>Ann Lee</dc:creator><dc:date>2024-01-01T00:00:00</dc:date><text:p>first<text:s text:c="2"/>line</text:p><text:p>second</text:p></office:annotation><text:p>value</text:p></table:table-cell>
                   <table:table-cell table:number-columns-repeated="3"><office:annotation><text:p>on an empty cell</text:p></office:annotation></table:table-cell>
                 </table:table-row>
                 <table:table-row table:number-rows-repeated="1048575"><table:table-cell><office:annotation><dc:creator>Bob</dc:creator><text:p>padding</text:p></office:annotation></table:table-cell></table:table-row>
               </table:table>"#,
        );
        let b = read_styled("", &xml, Limits::default());
        let s = &b.sheets[0];
        assert_eq!(s.cells[&(0, 0)].value, CellValue::Text("value".into()));
        assert_eq!(
            s.comments,
            [
                (
                    0,
                    0,
                    "Ann Lee".to_string(),
                    "first  line\nsecond".to_string()
                ),
                (0, 1, String::new(), "on an empty cell".to_string()),
                (1, 0, "Bob".to_string(), "padding".to_string()),
            ]
        );
    }

    /// The content validations of [`ods_content_validations`].
    const VALIDATIONS: &str = r#"<table:content-validations>
        <table:content-validation table:name="val1" table:condition="of:cell-content-is-whole-number() and cell-content-is-between(1;10)" table:allow-empty-cell="true" table:base-cell-address="S.B2">
          <table:help-message table:title="Number" table:display="true"><text:p>Enter</text:p><text:p>1-10</text:p></table:help-message>
          <table:error-message table:message-type="warning" table:title="Oops" table:display="true"><text:p>Out of range</text:p></table:error-message>
        </table:content-validation>
        <table:content-validation table:name="val2" table:condition="of:cell-content-is-in-list(&quot;a&quot;;&quot;b c&quot;)" table:allow-empty-cell="false" table:display-list="none" table:base-cell-address="S.A1"/>
        <table:content-validation table:name="val3" table:condition="of:is-true-formula([.A1]&gt;0)" table:base-cell-address="S.C2"/>
        <table:content-validation table:name="val4" table:condition="of:cell-content-is-decimal-number() and cell-content()&gt;=[.$Z$1]" table:base-cell-address="S.D1"/>
        <table:content-validation table:name="val5" table:condition="of:cell-content-text-length()&lt;=5" table:base-cell-address="S.E1"/>
        <table:content-validation table:name="val6" table:condition="of:cell-content-is-in-list([.$Y$1:.$Y$3])" table:base-cell-address="S.F1"/>
        <table:content-validation table:name="bad" table:condition="of:something-odd()" table:base-cell-address="S.A1"/>
      </table:content-validations>"#;

    /// The table of [`ods_content_validations`]: val1 on B2:B5 (a repeated
    /// row), val2 on A1:C1 (a repeated cell), val3 on C3:C4, val4 on D1,
    /// val5 on E1:E1048576 (padding), val6 on F1 and `bad` on G1.
    const VALIDATED: &str = r#"<table:table table:name="S">
        <table:table-row><table:table-cell table:content-validation-name="val2" table:number-columns-repeated="3"/><table:table-cell table:content-validation-name="val4"/><table:table-cell table:content-validation-name="val5"/><table:table-cell table:content-validation-name="val6"/><table:table-cell table:content-validation-name="bad"/></table:table-row>
        <table:table-row><table:table-cell/><table:table-cell table:content-validation-name="val1" office:value-type="float" office:value="3"/></table:table-row>
        <table:table-row table:number-rows-repeated="2"><table:table-cell/><table:table-cell table:content-validation-name="val1"/><table:table-cell table:content-validation-name="val3"/></table:table-row>
        <table:table-row><table:table-cell/><table:table-cell table:content-validation-name="val1"/></table:table-row>
        <table:table-row table:number-rows-repeated="1048571"><table:table-cell table:number-columns-repeated="4"/><table:table-cell table:content-validation-name="val5"/></table:table-row>
      </table:table>"#;

    /// Content validations are the sheet's rules over the cells that name
    /// them, their formulas moved from the base cell to the range's
    /// top-left; a condition this doesn't read is left out.
    #[test]
    fn ods_content_validations() {
        use crate::sheet::AlertStyle;
        let xml = styled_content("", "", &format!("{VALIDATIONS}{VALIDATED}"));
        let b = read_styled("", &xml, Limits::default());
        let v = &super::super::placed_dvs(&b.sheets[0]);
        let got: Vec<_> = v
            .iter()
            .map(|d| {
                (
                    d.kind.as_str(),
                    d.operator.as_str(),
                    d.formula1.as_str(),
                    d.formula2.as_str(),
                    d.ranges.clone(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("list", "", "\"a,b c\"", "", vec![(0, 0, 0, 2)]),
                (
                    "decimal",
                    "greaterThanOrEqual",
                    "$Z$1",
                    "",
                    vec![(0, 3, 0, 3)]
                ),
                (
                    "textLength",
                    "lessThanOrEqual",
                    "5",
                    "",
                    vec![(0, 4, 0, 4), (5, 4, 1048575, 4)]
                ),
                ("list", "", "$Y$1:$Y$3", "", vec![(0, 5, 0, 5)]),
                ("whole", "between", "1", "10", vec![(1, 1, 4, 1)]),
                // Based at C2, A1 is the row above; at C3, A2.
                ("custom", "", "A2>0", "", vec![(2, 2, 3, 2)]),
            ]
        );
        let w = &v[4];
        assert_eq!(
            (w.prompt_title.as_str(), w.prompt.as_deref()),
            ("Number", Some("Enter\n1-10"))
        );
        assert_eq!(
            (w.error_title.as_str(), w.error.as_str()),
            ("Oops", "Out of range")
        );
        assert!(w.show_input && w.show_error && w.allow_blank && w.show_dropdown);
        assert_eq!(w.error_style, AlertStyle::Warning);
        assert!(!v[0].allow_blank && !v[0].show_dropdown && !v[0].show_input);
    }

    #[test]
    fn ods_validation_conditions() {
        let c = |s: &str| validation_condition(s);
        let row = |k: &str, o: &str, a: &str, b: &str| {
            Some(Condition {
                kind: k.into(),
                operator: o.into(),
                formula1: a.into(),
                formula2: b.into(),
            })
        };
        assert_eq!(
            c("of:cell-content-is-date() and cell-content-is-not-between([.A1];[.A2])"),
            row("date", "notBetween", "A1", "A2")
        );
        assert_eq!(
            c("cell-content-text-length-is-between(1;5)"),
            row("textLength", "between", "1", "5")
        );
        assert_eq!(
            c("of:cell-content()!=3"),
            row("decimal", "notEqual", "3", "")
        );
        assert_eq!(
            c("of:is-true-formula(AND([.A1]>0;[.B1]<2))"),
            row("custom", "", "AND(A1>0,B1<2)", "")
        );
        assert_eq!(c("of:cell-content-is-whole-number()"), None);
        assert_eq!(c("of:cell-content-is-between(1)"), None);
        // No condition: any value.
        assert_eq!(c(""), row("", "", "", ""));
        assert_eq!(c("of:"), row("", "", "", ""));
    }

    /// The named styles of [`ods_style_map_and_calcext_cf`]: `Default` in
    /// black, `Good` bold on green and `Bad` italic red, both under it.
    const CF_STYLES: &str = r##"<office:document-styles xmlns:office="o" xmlns:style="s" xmlns:fo="f"><office:styles>
        <style:style style:name="Default" style:family="table-cell"><style:text-properties fo:color="#000000"/></style:style>
        <style:style style:name="Good" style:family="table-cell" style:parent-style-name="Default"><style:table-cell-properties fo:background-color="#00ff00"/><style:text-properties fo:font-weight="bold"/></style:style>
        <style:style style:name="Bad" style:family="table-cell" style:parent-style-name="Default"><style:text-properties fo:color="#ff0000" fo:font-style="italic"/></style:style>
        </office:styles></office:document-styles>"##;

    /// `style:map`s are conditional formats over the cells in their style,
    /// formulas moved from their base cell to the range's top-left; a
    /// table with LibreOffice's `calcext:conditional-format`s takes those
    /// instead (its maps are their copies). The applied style's own
    /// properties, short of `Default`, are the format.
    #[test]
    fn ods_style_map_and_calcext_cf() {
        use crate::sheet::Dxf;
        use crate::xlsx::NewCfRule;
        let xml = styled_content(
            "",
            r#"<style:style style:name="ce1" style:family="table-cell">
                 <style:map style:condition="cell-content()&gt;5" style:apply-style-name="Good" style:base-cell-address="S.B2"/>
                 <style:map style:condition="is-true-formula([.$A1]&gt;5)" style:apply-style-name="Bad" style:base-cell-address="S.B1"/>
                 <style:map style:condition="cell-content-is-duplicate()" style:apply-style-name="Bad" style:base-cell-address="S.B1"/>
               </style:style>"#,
            r#"<table:table table:name="S">
                 <table:table-row><table:table-cell/></table:table-row>
                 <table:table-row><table:table-cell/><table:table-cell table:style-name="ce1" table:number-columns-repeated="2"/></table:table-row>
                 <table:table-row table:number-rows-repeated="2"><table:table-cell/><table:table-cell table:style-name="ce1"/></table:table-row>
                 <table:table-row table:number-rows-repeated="1048572"><table:table-cell table:style-name="ce1"/></table:table-row>
               </table:table>
               <table:table table:name="T">
                 <table:table-row><table:table-cell table:style-name="ce1"/></table:table-row>
                 <calcext:conditional-formats><calcext:conditional-format calcext:target-range-address="T.A1:T.A3 T.C1">
                   <calcext:condition calcext:apply-style-name="Good" calcext:value="between(1,5)" calcext:base-cell-address="T.A1"/>
                   <calcext:condition calcext:apply-style-name="Bad" calcext:value="formula-is([.A2]&gt;0)" calcext:base-cell-address="T.A2"/>
                   <calcext:condition calcext:apply-style-name="Good" calcext:value="duplicate" calcext:base-cell-address="T.A1"/>
                 </calcext:conditional-format></calcext:conditional-formats>
               </table:table>"#,
        );
        let b = read_styled(CF_STYLES, &xml, Limits::default());
        let good = Dxf {
            fill: Some((0, 255, 0)),
            bold: Some(true),
            ..Dxf::default()
        };
        let bad = Dxf {
            color: Some((255, 0, 0)),
            italic: Some(true),
            ..Dxf::default()
        };
        let s = &b.sheets[0].cond_formats;
        assert_eq!(s.len(), 1);
        // B2:C2 and B3:B4; not the padding rows.
        assert_eq!(s[0].ranges, [(1, 1, 1, 2), (2, 1, 3, 1)]);
        assert_eq!(
            s[0].rules,
            [
                (
                    NewCfRule::CellIs {
                        op: "greaterThan".into(),
                        formulas: vec!["5".into()],
                    },
                    good.clone()
                ),
                (
                    NewCfRule::Expression {
                        formula: "$A2>5".into()
                    },
                    bad.clone()
                ),
            ]
        );
        let t = &b.sheets[1].cond_formats;
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].ranges, [(0, 0, 2, 0), (0, 2, 0, 2)]);
        assert_eq!(
            t[0].rules,
            [
                (
                    NewCfRule::CellIs {
                        op: "between".into(),
                        formulas: vec!["1".into(), "5".into()],
                    },
                    good
                ),
                (
                    NewCfRule::Expression {
                        formula: "A1>0".into()
                    },
                    bad
                ),
            ]
        );
    }

    #[test]
    fn ods_cf_conditions() {
        use crate::xlsx::NewCfRule;
        let cell_is = |op: &str, f: &[&str]| {
            Some(NewCfRule::CellIs {
                op: op.into(),
                formulas: f.iter().map(|s| s.to_string()).collect(),
            })
        };
        assert_eq!(
            cf_condition("cell-content-is-between(1;[.A1])"),
            cell_is("between", &["1", "A1"])
        );
        assert_eq!(
            cf_condition("not-between(1,5)"),
            cell_is("notBetween", &["1", "5"])
        );
        assert_eq!(cf_condition("!=0"), cell_is("notEqual", &["0"]));
        assert_eq!(
            cf_condition("<=[.$B$1]"),
            cell_is("lessThanOrEqual", &["$B$1"])
        );
        assert_eq!(
            cf_condition("cell-content()=\"x\""),
            cell_is("equal", &["\"x\""])
        );
        assert_eq!(cf_condition("top-elements(10)"), None);
        assert_eq!(cf_condition("begins-with(\"a\")"), None);
    }

    /// A run of styled blanks short of the grid's edge, repeated for
    /// almost every row (so neither padding test drops it), costs the
    /// styled-blank cap, not rows x blanks: once a copy no longer fits, a
    /// row of nothing else stops copying (#884 r1). Under the old loop this
    /// visited about 1.7e10 cells.
    #[test]
    fn ods_styled_blank_rows_stop_at_the_cap() {
        let auto = r##"<style:style style:name="ce1" style:family="table-cell"><style:text-properties fo:font-weight="bold"/></style:style>"##;
        let xml = styled_content(
            "",
            auto,
            r#"<table:table table:name="S">
                 <table:table-row table:number-rows-repeated="1048574"><table:table-cell table:style-name="ce1" table:number-columns-repeated="16383"/></table:table-row>
                 <table:table-row><table:table-cell office:value-type="float" office:value="1"/></table:table-row>
               </table:table>"#,
        );
        for (cap, rows) in [(16_383, 1), (40_000, 2), (10, 0)] {
            let limits = Limits {
                styled_blanks: cap,
                ..Limits::default()
            };
            let b = read_styled("", &xml, limits);
            let cells = &b.sheets[0].cells;
            assert_eq!(cells.len(), rows * 16_383 + 1, "cap {cap}");
            assert_eq!(cells[&(1_048_574, 0)].value, CellValue::Number(1.0));
        }
    }

    /// Range lists and base addresses on sheets whose quoted names hold a
    /// space, a quote or a colon (#884 r1).
    #[test]
    fn ods_quoted_sheet_addresses() {
        assert_eq!(
            range_list("'Q1 Sales'.A1:'Q1 Sales'.A10 'Q1 Sales'.C1 $'Bob''s'.$B$2:.$C$3 'A:B'.D4"),
            [(0, 0, 9, 0), (0, 2, 0, 2), (1, 1, 2, 2), (3, 3, 3, 3)]
        );
        assert_eq!(address_cell("$'Q1 Sales'.$B$2"), Some((1, 1)));
        assert_eq!(address_cell("'Bob''s'.C3"), Some((2, 2)));

        // A calcext conditional format and a validation's base cell on
        // `Q1 Sales`.
        let xml = styled_content(
            "",
            r##"<style:style style:name="Good" style:family="table-cell"><style:table-cell-properties fo:background-color="#00ff00"/></style:style>"##,
            r#"<table:content-validations><table:content-validation table:name="v" table:condition="of:is-true-formula([.A1]&gt;0)" table:base-cell-address="'Q1 Sales'.B1"/></table:content-validations>
               <table:table table:name="Q1 Sales">
                 <table:table-row><table:table-cell/></table:table-row>
                 <table:table-row><table:table-cell table:number-columns-repeated="2"/><table:table-cell table:content-validation-name="v"/></table:table-row>
                 <calcext:conditional-formats><calcext:conditional-format calcext:target-range-address="'Q1 Sales'.A1:'Q1 Sales'.A3">
                   <calcext:condition calcext:apply-style-name="Good" calcext:value="formula-is([.B2]&gt;0)" calcext:base-cell-address="'Q1 Sales'.A2"/>
                 </calcext:conditional-format></calcext:conditional-formats>
               </table:table>"#,
        );
        let b = read_styled("", &xml, Limits::default());
        let s = &b.sheets[0];
        // Based at B1, `[.A1]` is the cell to its left; on C2 (its
        // anchor), that is B2.
        let dv = &super::super::placed_dvs(s)[0];
        assert_eq!(dv.ranges, [(1, 2, 1, 2)]);
        assert_eq!(dv.formula1, "B2>0");
        assert_eq!(s.cond_formats[0].ranges, [(0, 0, 2, 0)]);
        assert_eq!(
            s.cond_formats[0].rules[0].0,
            crate::xlsx::NewCfRule::Expression {
                formula: "B1>0".into()
            }
        );
    }

    /// A validation with no condition, or an empty one, is an any-value
    /// rule: its messages and flags are kept, as `.xls`/`.xlsb` keep a
    /// valType 0 rule (#884 r1).
    #[test]
    fn ods_validation_without_a_condition_keeps_its_messages() {
        let xml = styled_content(
            "",
            "",
            r#"<table:content-validations>
                 <table:content-validation table:name="v1" table:allow-empty-cell="true" table:base-cell-address="S.A1">
                   <table:help-message table:title="Tip" table:display="true"><text:p>Type a name</text:p></table:help-message>
                 </table:content-validation>
                 <table:content-validation table:name="v2" table:condition="" table:base-cell-address="S.A1"/>
               </table:content-validations>
               <table:table table:name="S"><table:table-row><table:table-cell table:content-validation-name="v1"/><table:table-cell table:content-validation-name="v2"/></table:table-row></table:table>"#,
        );
        let b = read_styled("", &xml, Limits::default());
        let v = &super::super::placed_dvs(&b.sheets[0]);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].kind.as_str(), v[0].formula1.as_str()), ("", ""));
        assert_eq!(
            (v[0].prompt_title.as_str(), v[0].prompt.as_deref()),
            ("Tip", Some("Type a name"))
        );
        assert!(v[0].show_input && v[0].allow_blank);
        assert_eq!(
            (v[1].kind.as_str(), v[1].ranges.clone()),
            ("", vec![(0, 1, 0, 1)])
        );
    }

    /// A rule over D2 and B5 is anchored at B2, the least row and column
    /// of its ranges, which is in neither: its formulas move there from
    /// their base cell, D2 (#884 r2).
    #[test]
    fn ods_rules_are_anchored_at_their_least_corner() {
        let xml = styled_content(
            "",
            r##"<style:style style:name="Good" style:family="table-cell"><style:table-cell-properties fo:background-color="#00ff00"/></style:style>"##,
            r#"<table:content-validations><table:content-validation table:name="v" table:condition="of:is-true-formula([.D2]&gt;0)" table:base-cell-address="S.D2"/></table:content-validations>
               <table:table table:name="S">
                 <table:table-row><table:table-cell/></table:table-row>
                 <table:table-row><table:table-cell table:number-columns-repeated="3"/><table:table-cell table:content-validation-name="v" office:value-type="float" office:value="5"/></table:table-row>
                 <table:table-row table:number-rows-repeated="2"><table:table-cell/></table:table-row>
                 <table:table-row><table:table-cell/><table:table-cell table:content-validation-name="v" office:value-type="float" office:value="-1"/></table:table-row>
                 <calcext:conditional-formats><calcext:conditional-format calcext:target-range-address="S.D2 S.B5">
                   <calcext:condition calcext:apply-style-name="Good" calcext:value="formula-is([.D2]&gt;0)" calcext:base-cell-address="S.D2"/>
                 </calcext:conditional-format></calcext:conditional-formats>
               </table:table>"#,
        );
        let (pkg, _) = super::super::open_workbook(&ods_zip(&xml)).unwrap();
        let back = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        let wb = &back.workbook;
        let dv = &wb.sheets[0].validations[0];
        assert_eq!(dv.ranges, [(1, 3, 1, 3), (4, 1, 4, 1)]);
        assert_eq!(dv.formula1, "B2>0");
        assert_eq!(crate::validation::invalid_cells(wb, 0, None), [(4, 1)]);
        assert!(crate::cf::cell_dxf(wb, 0, 1, 3).is_some());
        assert!(crate::cf::cell_dxf(wb, 0, 4, 1).is_none());
    }

    /// Empty cells walk the column styles they cross only when one gives
    /// them something to keep, and only within `Limits::column_walk`, so
    /// rows x column elements can't grow the import's time; adjacent
    /// columns in one style are one step (#884 r2).
    #[test]
    fn ods_column_style_walk_is_bounded() {
        let auto = r##"<style:style style:name="ce1" style:family="table-cell"><style:table-cell-properties fo:background-color="#00ff00"/></style:style>
            <style:style style:name="ce2" style:family="table-cell"/>
            <style:style style:name="ce3" style:family="table-cell"/>"##;
        let table = |a: &str, b: &str| {
            let cols: String = (0..16384)
                .map(|i| {
                    format!(
                        r#"<table:table-column table:default-cell-style-name="{}"/>"#,
                        if i % 2 == 0 { a } else { b }
                    )
                })
                .collect();
            let rows = r#"<table:table-row><table:table-cell table:number-columns-repeated="16383"/></table:table-row>"#.repeat(5);
            styled_content(
                "",
                auto,
                &format!(r#"<table:table table:name="S">{cols}{rows}</table:table>"#),
            )
        };
        let read = |xml: &str, walk: usize| {
            let limits = Limits {
                column_walk: walk,
                ..Limits::default()
            };
            let b = read_styled("", xml, limits);
            let cells = &b.sheets[0].cells;
            let rows: std::collections::BTreeSet<u32> = cells.keys().map(|k| k.0).collect();
            (cells.len(), rows.into_iter().collect::<Vec<_>>())
        };
        // Alternating styles: each row's run is 16383 steps. The budget
        // lasts two rows; the rows after it keep no styled blanks.
        assert_eq!(read(&table("ce1", "ce2"), 20_000), (2 * 8192, vec![0, 1]));
        // One style throughout is one step a row.
        assert_eq!(read(&table("ce1", "ce1"), 2), (2 * 16383, vec![0, 1]));
        // No column style gives a blank anything to keep: no walk at all.
        assert_eq!(read(&table("ce2", "ce3"), 0), (0, vec![]));
        assert_eq!(read(&table("ce2", "ce3"), 4_000_000), (0, vec![]));
    }

    /// A cell's text, an annotation's and a validation message's read the
    /// same paragraph elements: spaces, tabs and line breaks too (#884 r2).
    #[test]
    fn ods_message_and_annotation_text_elements() {
        let xml = styled_content(
            "",
            "",
            r#"<table:content-validations><table:content-validation table:name="v" table:base-cell-address="S.A1">
                 <table:help-message table:display="true">
                   <text:p>a<text:s/>b<text:line-break/>c</text:p>
                   <text:p>d<text:s text:c="2"/>e</text:p>
                 </table:help-message>
               </table:content-validation></table:content-validations>
               <table:table table:name="S"><table:table-row>
                 <table:table-cell table:content-validation-name="v" office:value-type="string"><office:annotation><dc:creator>Ann</dc:creator><text:p>x<text:tab/>y<text:s/>z<text:line-break/>w</text:p></office:annotation><text:p>own<text:tab/>text</text:p></table:table-cell>
               </table:table-row></table:table>"#,
        );
        let b = read_styled("", &xml, Limits::default());
        let s = &b.sheets[0];
        assert_eq!(
            super::super::placed_dvs(s)[0].prompt.as_deref(),
            Some("a b\nc\nd  e")
        );
        assert_eq!(
            s.comments,
            [(0, 0, "Ann".to_string(), "x\ty z\nw".to_string())]
        );
        assert_eq!(s.cells[&(0, 0)].value, CellValue::Text("own\ttext".into()));
    }

    /// A styles part with these automatic styles (page layouts) and
    /// master pages.
    fn styles_part(auto: &str, masters: &str) -> String {
        format!(
            r#"<office:document-styles xmlns:office="o" xmlns:style="s" xmlns:fo="f" xmlns:svg="g" xmlns:text="x" xmlns:loext="l"><office:automatic-styles>{auto}</office:automatic-styles><office:master-styles>{masters}</office:master-styles></office:document-styles>"#
        )
    }

    /// Page layouts and master pages for four tables (#901): `mA` with
    /// every field away from its default and text in every header part,
    /// `mB` on `loext:` names with its header not displayed, and `Default`.
    fn page_styles() -> String {
        styles_part(
            r#"<style:page-layout style:name="pmA"><style:page-layout-properties fo:page-width="297mm" fo:page-height="210mm" fo:margin-left="0.5in" fo:margin-right="0.6in" fo:margin-top="0.4in" fo:margin-bottom="0.3in" style:print-orientation="landscape" style:scale-to="80%" style:print-page-order="ltr" style:first-page-number="3" style:table-centering="both" style:print="objects grid headers annotations charts"/>
                 <style:header-style><style:header-footer-properties fo:min-height="0.5in" fo:margin-bottom="0.1in"/></style:header-style>
                 <style:footer-style><style:header-footer-properties svg:height="0.25in" fo:margin-top="0.05in"/></style:footer-style></style:page-layout>
               <style:page-layout style:name="pmB"><style:page-layout-properties fo:page-width="8.5in" fo:page-height="11in" fo:margin-top="0.7in" fo:margin-bottom="0.7in" loext:scale-to-X="2" loext:scale-to-Y="3" style:first-page-number="continue" style:print-page-order="ttb" style:table-centering="none"/>
                 <style:header-style><style:header-footer-properties fo:min-height="0.4in"/></style:header-style>
                 <style:footer-style><style:header-footer-properties fo:min-height="0.2in"/></style:footer-style></style:page-layout>
               <style:page-layout style:name="pmC"><style:page-layout-properties fo:margin-left="1in" fo:margin-top="oops"/></style:page-layout>"#,
            r#"<style:master-page style:name="mA" style:page-layout-name="pmA">
                 <style:header><style:region-left><text:p>Page <text:page-number>1</text:page-number> of <text:page-count>9</text:page-count></text:p></style:region-left><style:region-center><text:p><text:sheet-name>???</text:sheet-name> &amp; <text:span>Co</text:span></text:p></style:region-center><style:region-right><text:p><text:date>2026-01-01</text:date> <text:time>10:00</text:time></text:p></style:region-right></style:header>
                 <style:header-left><text:p>Even</text:p></style:header-left>
                 <style:header-first/>
                 <style:footer><text:p>Line1</text:p><text:p><text:file-name text:display="full">a.ods</text:file-name><text:s text:c="2"/>end<text:tab/>x<text:line-break/>y <text:title>Report</text:title></text:p></style:footer>
                 <style:footer-left style:display="false"><text:p>not shown</text:p></style:footer-left>
                 <loext:footer-first><text:p>First</text:p></loext:footer-first>
               </style:master-page>
               <style:master-page style:name="mB" style:page-layout-name="pmB"><style:header style:display="false"><text:p>hidden</text:p></style:header><style:footer/><style:header-first/><style:footer-first/></style:master-page>
               <style:master-page style:name="Default" style:page-layout-name="pmC"/>"#,
        )
    }

    /// Four tables on `mA`, `mB`, a master page that isn't there and none.
    fn page_content() -> String {
        styled_content(
            "",
            r#"<style:style style:name="ta1" style:family="table" style:master-page-name="mA"/><style:style style:name="ta2" style:family="table" style:master-page-name="mB"/><style:style style:name="ta3" style:family="table" style:master-page-name="Missing"/>"#,
            r#"<table:table table:name="A" table:style-name="ta1"><table:table-row><table:table-cell office:value-type="float" office:value="1"/></table:table-row></table:table>
               <table:table table:name="B" table:style-name="ta2"/>
               <table:table table:name="C" table:style-name="ta3"/>
               <table:table table:name="D"/>"#,
        )
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// An `.ods` sheet's page setup comes from its table style's master page
    /// and that page's layout (#901): margins with the header and footer's
    /// room, paper, orientation, scale or fit, page order, first page,
    /// centring, print options and the header and footer text in Excel's
    /// codes.
    #[test]
    fn ods_reads_page_layout_and_header_footer() {
        let b = read_styled(&page_styles(), &page_content(), Limits::default());
        let a = &b.sheets[0].page_setup;
        let m = a.margins;
        for (got, want) in [
            (m.left, 0.5),
            (m.right, 0.6),
            (m.header, 0.4),
            (m.top, 1.0),
            (m.footer, 0.3),
            (m.bottom, 0.6),
        ] {
            assert!(close(got, want), "{m:?}");
        }
        assert_eq!(a.paper_size, 9);
        assert_eq!(a.orientation, Orientation::Landscape);
        assert_eq!(a.scale, 80);
        assert!(!a.fit_to_page);
        assert_eq!(a.page_order, PageOrder::OverThenDown);
        assert_eq!(a.first_page_number, Some(3));
        assert!(a.h_centered && a.v_centered && a.grid_lines && a.headings);
        assert_eq!(a.cell_comments, CellComments::AtEnd);
        let hf = &a.header_footer;
        assert_eq!(
            hf.odd_header.as_deref(),
            Some("&LPage &P of &N&C&A && Co&R&D &T")
        );
        assert_eq!(
            hf.odd_footer.as_deref(),
            Some("&CLine1\n&Z&F  end x\ny Report")
        );
        assert!(hf.different_odd_even && hf.different_first);
        assert_eq!(hf.even_header.as_deref(), Some("&CEven"));
        assert_eq!(hf.even_footer, None);
        assert_eq!(hf.first_header, None);
        assert_eq!(hf.first_footer.as_deref(), Some("&CFirst"));
        assert!(hf.scale_with_doc && hf.align_with_margins);

        // mB: its header isn't displayed, so the body starts at the page
        // margin (top == header); its shown, empty footer still takes room.
        let b2 = &b.sheets[1].page_setup;
        let m = b2.margins;
        assert!(close(m.top, 0.7) && close(m.header, 0.7), "{m:?}");
        assert!(close(m.bottom, 0.9) && close(m.footer, 0.7), "{m:?}");
        assert!(close(m.left, 0.7) && close(m.right, 0.7), "{m:?}");
        assert_eq!(b2.paper_size, 1);
        assert_eq!(b2.orientation, Orientation::Default);
        assert!(b2.fit_to_page);
        assert_eq!((b2.fit_width, b2.fit_height, b2.scale), (2, 3, 100));
        assert_eq!(b2.first_page_number, None);
        assert_eq!(b2.page_order, PageOrder::DownThenOver);
        assert_eq!(b2.header_footer, Default::default());

        // A master page that isn't there, and no table style: `Default`'s,
        // whose unreadable top margin is left out.
        let want = PageSetup {
            margins: crate::print::setup::Margins {
                left: 1.0,
                ..Default::default()
            },
            ..PageSetup::default()
        };
        assert_eq!(b.sheets[2].page_setup, want);
        assert_eq!(b.sheets[3].page_setup, want);

        // No master pages at all: the model's default.
        let b = read_styled("", &page_content(), Limits::default());
        assert_eq!(b.sheets[0].page_setup, PageSetup::default());
    }

    /// Rows and columns in a style with `fo:break-before="page"` are manual
    /// page breaks (#901), each row (column) of a repeated run, row and
    /// column 0 and anything past the grid left out, at most
    /// `page::MAX_BREAKS` however long the run.
    #[test]
    fn ods_reads_row_and_column_breaks() {
        let xml = styled_content(
            "",
            r#"<style:style style:name="ro1" style:family="table-row"><style:table-row-properties fo:break-before="page"/></style:style><style:style style:name="ro2" style:family="table-row"><style:table-row-properties fo:break-before="auto"/></style:style><style:style style:name="co1" style:family="table-column"><style:table-column-properties fo:break-before="page" style:column-width="1in"/></style:style><style:style style:name="co2" style:family="table-column"><style:table-column-properties style:column-width="1in"/></style:style>"#,
            r#"<table:table table:name="S">
                 <table:table-column table:style-name="co1"/><table:table-column table:style-name="co2"/><table:table-column table:style-name="co1"/><table:table-column table:style-name="co2" table:number-columns-repeated="2"/><table:table-column table:style-name="co1" table:number-columns-repeated="2"/>
                 <table:table-row table:style-name="ro1"><table:table-cell office:value-type="float" office:value="1"/></table:table-row>
                 <table:table-row table:style-name="ro2" table:number-rows-repeated="2"/>
                 <table:table-row table:style-name="ro1"/>
                 <table:table-row table:style-name="ro2"/>
                 <table:table-row table:style-name="ro1" table:number-rows-repeated="1048571"/>
               </table:table>
               <table:table table:name="T"><table:table-column table:number-columns-repeated="16384"/><table:table-column table:style-name="co1"/><table:table-row table:number-rows-repeated="1048576"/><table:table-row table:style-name="ro1"/></table:table>"#,
        );
        let started = std::time::Instant::now();
        let b = read_styled("", &xml, Limits::default());
        assert!(started.elapsed().as_secs() < 5, "a long run is walked");
        let s = &b.sheets[0];
        let ids = |v: &[crate::sheet::PageBreak]| v.iter().map(|b| b.id).collect::<Vec<_>>();
        assert_eq!(ids(&s.col_breaks), [2, 5, 6]);
        assert!(
            s.col_breaks
                .iter()
                .all(|b| b.attrs == r#" max="1048575" man="1""#)
        );
        assert_eq!(s.row_breaks.len(), super::super::page::MAX_BREAKS);
        assert_eq!(ids(&s.row_breaks[..3]), [3, 5, 6]);
        assert_eq!(
            s.row_breaks.last().unwrap().id,
            5 + super::super::page::MAX_BREAKS as u32 - 2
        );
        assert!(
            s.row_breaks
                .iter()
                .all(|b| b.attrs == r#" max="16383" man="1""#)
        );
        let t = &b.sheets[1];
        assert!(t.row_breaks.is_empty() && t.col_breaks.is_empty());
    }

    fn ods_zip_styled(styles: &str, content: &str) -> Vec<u8> {
        opccore::zipwrite::write_zip(&[
            (
                "mimetype".to_string(),
                b"application/vnd.oasis.opendocument.spreadsheet".to_vec(),
            ),
            ("styles.xml".to_string(), styles.as_bytes().to_vec()),
            ("content.xml".to_string(), content.as_bytes().to_vec()),
        ])
    }

    /// A table's `table:print-ranges` is its sheet's `_xlnm.Print_Area` and
    /// its header rows and columns its `_xlnm.Print_Titles` (#901), as
    /// Excel spells them, and they come through Convert.
    #[test]
    fn ods_reads_print_area_and_titles() {
        use crate::print::area::{PrintTitles, print_area, print_titles};
        let xml = styled_content(
            "",
            "",
            r#"<table:table table:name="My Sheet" table:print-ranges="'My Sheet'.A1:'My Sheet'.C5 $'My Sheet'.$E$1:.$F$2"><table:table-row><table:table-cell office:value-type="float" office:value="1"/></table:table-row></table:table>
               <table:table table:name="T2"><table:table-header-columns><table:table-column table:number-columns-repeated="2"/></table:table-header-columns><table:table-column/><table:table-row/><table:table-header-rows><table:table-row table:number-rows-repeated="2"/></table:table-header-rows><table:table-row/></table:table>
               <table:table table:name="T3" table:print-ranges="T3.A1:T3.B2 nonsense"/>
               <table:table table:name="T4" table:print-ranges="T4.A1:T4.B1048576"><table:table-header-rows><table:table-row/></table:table-header-rows></table:table>"#,
        );
        let b = read_styled("", &xml, Limits::default());
        let names: Vec<_> = b
            .names
            .iter()
            .map(|d| (d.name.as_str(), d.scope, d.formula.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                (
                    "_xlnm.Print_Area",
                    Some(0),
                    "'My Sheet'!$A$1:$C$5,'My Sheet'!$E$1:$F$2"
                ),
                // T2 and T4 are cell addresses, so their names are quoted.
                ("_xlnm.Print_Titles", Some(1), "'T2'!$A:$B,'T2'!$2:$3"),
                ("_xlnm.Print_Area", Some(3), "'T4'!$A:$B"),
                ("_xlnm.Print_Titles", Some(3), "'T4'!$1:$1"),
            ]
        );
        let (pkg, _) = super::super::open_workbook(&ods_zip(&xml)).unwrap();
        let back = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        let wb = &back.workbook;
        assert_eq!(print_area(wb, 0), [(0, 0, 4, 2), (0, 4, 1, 5)]);
        assert_eq!(
            print_titles(wb, 1),
            PrintTitles {
                rows: Some((1, 2)),
                cols: Some((0, 1)),
            }
        );
        assert!(print_area(wb, 2).is_empty());
        assert_eq!(print_area(wb, 3), [(0, 0, MAX_ROWS - 1, 1)]);
        let kept: Vec<_> = wb
            .defined_names
            .iter()
            .map(|d| (d.name.as_str(), d.scope, d.formula.as_str()))
            .collect();
        assert_eq!(kept, names);
    }

    /// A table named with a `:` is renamed (`A:B` → `A_B`), and its print
    /// area and titles follow it (#901): spelled with the build's colon
    /// marker, as every other reference the reader builds.
    #[test]
    fn a_colon_named_tables_print_area_and_titles_follow_its_rename() {
        use crate::print::area::{PrintTitles, print_area, print_titles};
        let xml = styled_content(
            "",
            "",
            r#"<table:table table:name="A:B" table:print-ranges="'A:B'.A1:'A:B'.C5"><table:table-header-rows><table:table-row table:number-rows-repeated="2"/></table:table-header-rows><table:table-row><table:table-cell office:value-type="float" office:value="1"/></table:table-row></table:table>"#,
        );
        let (pkg, _) = super::super::open_workbook(&ods_zip(&xml)).unwrap();
        let wb = &pkg.workbook;
        assert_eq!(wb.sheets[0].name, "A_B");
        let names: Vec<_> = wb
            .defined_names
            .iter()
            .map(|d| (d.name.as_str(), d.scope, d.formula.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("_xlnm.Print_Area", Some(0), "A_B!$A$1:$C$5"),
                ("_xlnm.Print_Titles", Some(0), "A_B!$1:$2"),
            ]
        );
        assert_eq!(print_area(wb, 0), [(0, 0, 4, 2)]);
        assert_eq!(
            print_titles(wb, 0),
            PrintTitles {
                rows: Some((0, 1)),
                cols: None,
            }
        );
    }

    /// Side effect (#901): an imported `.ods`'s page setup and manual breaks
    /// come through Convert (a save as `.xlsx` and a reload).
    #[test]
    fn ods_page_setup_survives_convert() {
        let content = styled_content(
            "",
            r#"<style:style style:name="ta1" style:family="table" style:master-page-name="mA"/><style:style style:name="ro1" style:family="table-row"><style:table-row-properties fo:break-before="page"/></style:style><style:style style:name="co1" style:family="table-column"><style:table-column-properties fo:break-before="page"/></style:style>"#,
            r#"<table:table table:name="A" table:style-name="ta1"><table:table-column/><table:table-column table:style-name="co1"/><table:table-row><table:table-cell office:value-type="float" office:value="1"/></table:table-row><table:table-row table:style-name="ro1"/></table:table>"#,
        );
        let (pkg, _) =
            super::super::open_workbook(&ods_zip_styled(&page_styles(), &content)).unwrap();
        let imported = pkg.workbook.sheets[0].clone();
        assert_eq!(imported.page_setup.paper_size, 9);
        let back = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        let s = &back.workbook.sheets[0];
        assert_eq!(s.page_setup, imported.page_setup);
        let ids = |b: &[crate::sheet::PageBreak]| {
            b.iter().map(|b| (b.id, b.is_manual())).collect::<Vec<_>>()
        };
        assert_eq!(ids(&s.row_breaks), [(1, true)]);
        assert_eq!(ids(&s.col_breaks), [(1, true)]);
    }
}
