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

use super::{BookIn, ColIn, Limits, OpenError, SheetIn, sheet_prefix};
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
}

/// The data styles, table-cell styles and column styles of a styles or
/// content part.
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
                        ("style:table-column-properties", _) if column_style.is_some() => {
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
                    if text_into.is_some() {
                        XmlParser::append_decoded(p.text(), &mut text);
                    }
                }
                Event::End => {
                    if matches!(p.name(), "style:style" | "style:default-style") {
                        cell_style = None;
                        column_style = None;
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

    /// The model format of cell style `name` (`None`: no style).
    fn xf(&self, name: Option<&str>) -> Xf {
        self.props(name).xf(self.code(name.unwrap_or("Default")))
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
    /// Its annotation (#884): (author, text).
    comment: Option<(String, String)>,
}

/// An `office:annotation` being read: its author (`dc:creator`), its text
/// (each `text:p` a line) and how many paragraphs it has had.
#[derive(Default)]
struct NoteIn {
    author: String,
    text: String,
    paragraphs: usize,
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

/// A condition as the model holds it: (kind, operator, formula1, formula2)
/// in Excel's syntax, relative to the base cell. `kind` is a validation's
/// (`whole`, `list`, `custom`, ...; `""` when the condition names none),
/// or for a conditional format `cellIs` and `expression` (with
/// `formula1` its formula).
type Condition = (String, String, String, String);

/// A validation's `table:condition` (`of:cell-content-is-whole-number()
/// and cell-content-is-between(1;10)`, `cell-content-is-in-list("a";"b")`,
/// `is-true-formula([.A1]>0)`, ...). `None` for one this doesn't read.
fn validation_condition(cond: &str) -> Option<Condition> {
    let body = match cond.split_once(':') {
        Some((ns, rest)) if ns.bytes().all(|b| b.is_ascii_alphabetic()) => rest,
        _ => cond,
    };
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
    Some((kind, op, f1.unwrap_or_default(), f2.unwrap_or_default()))
}

/// An ODF cell address (`Sheet1.A1`, `$'My Sheet'.$B$2`) as (row, col).
fn address_cell(s: &str) -> Option<(u32, u32)> {
    let (_, cell) = split_address(s)?;
    crate::sheet::parse_cell_name(&cell)
}

/// `f`, its relative references moved from being based at `base` to
/// being based at `at` (an `.xlsx` bases them at the range's top-left).
fn rebase(f: &str, base: Option<(u32, u32)>, at: (u32, u32)) -> Option<String> {
    let Some(base) = base.filter(|&b| b != at && !f.is_empty()) else {
        return Some(f.to_string());
    };
    let (dr, dc) = (
        i64::from(at.0) - i64::from(base.0),
        i64::from(at.1) - i64::from(base.1),
    );
    crate::formula::translate_formula(f, dr, dc)
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
    let mut message: Option<(bool, String, usize)> = None;
    let mut row_rules: Vec<(u32, u32, String)> = Vec::new();
    let mut rule_cells: Vec<(String, Ranges)> = Vec::new();
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
                            ..SheetIn::default()
                        })?;
                        col_styles.clear();
                        col_runs.clear();
                        rule_cells.clear();
                        next_col = 0;
                        row = 0;
                    }
                    "table:table-column" => {
                        let n = p
                            .attr("table:number-columns-repeated")
                            .parse()
                            .unwrap_or(1u32);
                        col_styles.push((next_col, attr("table:default-cell-style-name")));
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
                    "office:annotation" if cell.is_some() && note.is_none() => {
                        hidden += 1;
                        note = Some((stack.len(), NoteIn::default()));
                    }
                    n if n == "office:annotation" || n.starts_with("draw:") => hidden += 1,
                    // An annotation's own paragraphs and spacing.
                    "text:p" if note.is_some() && hidden == 1 => {
                        if let Some((_, n)) = note.as_mut() {
                            if n.paragraphs > 0 {
                                n.text.push('\n');
                            }
                            n.paragraphs += 1;
                        }
                    }
                    "text:s" if note.is_some() && hidden == 1 => {
                        if let Some((_, n)) = note.as_mut() {
                            let k = p.attr("text:c").parse().unwrap_or(1usize).min(1 << 12);
                            n.text.push_str(&" ".repeat(k));
                        }
                    }
                    "text:line-break" if note.is_some() && hidden == 1 => {
                        if let Some((_, n)) = note.as_mut() {
                            n.text.push('\n');
                        }
                    }
                    "text:p" if hidden == 0 && message.is_none() => {
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
                    "table:content-validation" => {
                        let mut rule = crate::sheet::DataValidation {
                            allow_blank: p.attr("table:allow-empty-cell") != "false",
                            show_dropdown: p.attr("table:display-list") != "none",
                            ..crate::sheet::DataValidation::default()
                        };
                        let condition = validation_condition(&attr("table:condition"));
                        if let Some((kind, op, ..)) = &condition {
                            rule.kind = kind.clone();
                            rule.operator = op.clone();
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
                            message = Some((error, String::new(), 0));
                        }
                    }
                    "text:p" if message.is_some() => {
                        if let Some((_, text, n)) = message.as_mut() {
                            if *n > 0 {
                                text.push('\n');
                            }
                            *n += 1;
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
                if let Some((depth, n)) = note.as_mut().filter(|_| hidden == 1) {
                    let inside = &stack[*depth..];
                    if inside.iter().any(|e| e == "dc:creator") {
                        XmlParser::append_decoded(p.text(), &mut n.author);
                    } else if inside.iter().any(|e| e == "text:p") {
                        XmlParser::append_decoded(p.text(), &mut n.text);
                    }
                }
                if let Some((_, text, _)) = message.as_mut() {
                    XmlParser::append_decoded(p.text(), text);
                }
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
                        if note.as_ref().is_some_and(|(d, _)| *d == stack.len()) {
                            if let (Some((_, n)), Some(c)) = (note.take(), cell.as_mut()) {
                                c.comment = Some((n.author.trim().to_string(), n.text));
                            }
                        }
                    }
                    "table:table" => {
                        if let Some(sheet) = book.sheets.last_mut() {
                            table_columns(&col_runs, sheet);
                            for (name, cells) in rule_cells.drain(..) {
                                let Some(v) = validations.get(&name) else {
                                    continue;
                                };
                                let Some((_, _, f1, f2)) = &v.condition else {
                                    continue;
                                };
                                let Some(&(r, c, ..)) = cells.rects.first() else {
                                    continue;
                                };
                                let (Some(f1), Some(f2)) =
                                    (rebase(f1, v.base, (r, c)), rebase(f2, v.base, (r, c)))
                                else {
                                    continue;
                                };
                                sheet.validations.push(crate::sheet::DataValidation {
                                    ranges: cells.rects,
                                    formula1: f1,
                                    formula2: f2,
                                    ..v.rule.clone()
                                });
                            }
                        }
                    }
                    "table:content-validation" => {
                        if let Some((name, v)) = validation.take() {
                            validations.insert(name, v);
                        }
                    }
                    "table:help-message" | "table:error-message" => {
                        if let (Some((error, text, _)), Some((_, v))) =
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
                        } else if name == "table:table-cell" && last < MAX_COLS {
                            // A styled blank (#884), unless its run reaches
                            // the grid's last column: that is padding.
                            let runs = match &own {
                                Some(s) => vec![(col, last, Some(s.clone()))],
                                None => by_column(col, last),
                            };
                            for (from, to, style) in runs {
                                let look = look_of(style.as_deref(), &mut book);
                                if look.styled && book.take_styled_blanks((to - from) as usize) {
                                    let made = Cell {
                                        style: look.style,
                                        ..Cell::default()
                                    };
                                    row_cells.extend((from..to).map(|cc| (cc, made.clone())));
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
                            // The first instance was charged cell by cell;
                            // each further row copies them all.
                            let extra = (last.saturating_sub(row) as usize).saturating_sub(1);
                            let blanks = row_cells
                                .iter()
                                .filter(|(_, c)| c.value == CellValue::Empty)
                                .count();
                            let values = row_cells.len() - blanks;
                            let row_bytes: usize = row_cells
                                .iter()
                                .filter(|(_, c)| c.value != CellValue::Empty)
                                .map(|(_, c)| copy_bytes(c))
                                .sum();
                            let copies = extra.saturating_mul(values);
                            book.charge_repeats(copies, extra.saturating_mul(row_bytes))?;
                            book.charge_cells(copies)?;
                            let merge_copies = extra.saturating_mul(row_merges.len());
                            book.charge_repeats(merge_copies, 0)?;
                            for r in row..last {
                                // A copy's styled blanks, while they fit.
                                let with_blanks =
                                    r == row || (blanks > 0 && book.take_styled_blanks(blanks));
                                let Some(sheet) = book.sheets.last_mut() else {
                                    break;
                                };
                                for (cc, made) in &row_cells {
                                    if with_blanks || made.value != CellValue::Empty {
                                        sheet.cells.insert((r, *cc), made.clone());
                                    }
                                }
                                for &(c, cs, rs) in &row_merges {
                                    let r2 = r.saturating_add(rs - 1).min(MAX_ROWS - 1);
                                    let c2 = c.saturating_add(cs - 1).min(MAX_COLS - 1);
                                    sheet.merges.push((r, c, r2, c2));
                                }
                            }
                        }
                        // The cells naming a validation rule: one range per
                        // run, however many rows repeat it.
                        if row < MAX_ROWS {
                            for (c1, c2, rule) in row_rules.drain(..) {
                                let at = match rule_cells.iter().position(|(n, _)| *n == rule) {
                                    Some(i) => i,
                                    None => {
                                        rule_cells.push((rule, Ranges::default()));
                                        rule_cells.len() - 1
                                    }
                                };
                                rule_cells[at].1.add(row, last - 1, c1, c2 - 1);
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
               <style:style style:name="co2" style:family="table-column"><style:table-column-properties style:column-width="1.5in"/></style:style>"##,
            r#"<table:content-validations><table:content-validation table:name="v1" table:condition="of:is-true-formula([.A1]&gt;0)" table:base-cell-address="S.A1"/></table:content-validations>
               <table:table table:name="S">
                 <table:table-column table:style-name="co1"/>
                 <table:table-column table:style-name="co2"/>
                 <table:table-column table:style-name="co1" table:number-columns-repeated="16382"/>
                 <table:table-row><table:table-cell table:style-name="ce1" office:value-type="float" office:value="1"/><table:table-cell table:content-validation-name="v1" table:number-columns-repeated="2"/></table:table-row>
                 <table:table-row><table:table-cell><office:annotation><dc:creator>Ann</dc:creator><text:p>a note</text:p></office:annotation></table:table-cell></table:table-row>
                 <table:table-row><table:table-cell table:number-columns-spanned="2" table:number-rows-spanned="2"/></table:table-row>
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
        let v = &b.sheets[0].validations;
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
            Some((k.to_string(), o.to_string(), a.to_string(), b.to_string()))
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
        assert_eq!(c(""), None);
    }
}
