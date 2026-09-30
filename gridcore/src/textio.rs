//! Delimited and fixed-width text, read and written the way Excel does.
//!
//! One reader serves opening a `.csv`, the Text Import Wizard and Text to
//! Columns: [`split_text`] / [`split_value`] cut text into fields under a
//! [`TextParse`], and [`convert_field`] turns each field into a cell as if it
//! had been typed ([`crate::entry::parse_entry`]), under the column's
//! [`ColFormat`], the Advanced separators and File › Options › Data's
//! [`AutoConvert`] switches. The writer ([`sheet_text`], [`encode`],
//! [`sheet_prn`], [`web_page`]) produces Excel's Save As text types.

use crate::entry::{self, Entry, EntryCtx};
use crate::sheet::{
    Cell, CellValue, NumFmt, Sheet, Styles, Xf, classify_format_code, format_with, parts_to_serial,
    serial_to_parts,
};

// ---------------------------------------------------------------------------
// Parse options
// ---------------------------------------------------------------------------

/// The order of a Date column's parts (the wizard's MDY/DMY/… list).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DateOrder {
    #[default]
    Mdy,
    Dmy,
    Ymd,
    Myd,
    Dym,
    Ydm,
}

impl DateOrder {
    pub const ALL: [DateOrder; 6] = [
        DateOrder::Mdy,
        DateOrder::Dmy,
        DateOrder::Ymd,
        DateOrder::Myd,
        DateOrder::Dym,
        DateOrder::Ydm,
    ];

    /// `"MDY"`, `"DMY"`, …
    pub fn name(self) -> &'static str {
        match self {
            DateOrder::Mdy => "MDY",
            DateOrder::Dmy => "DMY",
            DateOrder::Ymd => "YMD",
            DateOrder::Myd => "MYD",
            DateOrder::Dym => "DYM",
            DateOrder::Ydm => "YDM",
        }
    }

    pub fn parse(s: &str) -> Option<DateOrder> {
        DateOrder::ALL
            .into_iter()
            .find(|o| o.name().eq_ignore_ascii_case(s.trim()))
    }
}

/// A column's data format: the wizard's step 3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColFormat {
    /// Numbers, dates and formulas convert as typed entry does.
    #[default]
    General,
    /// Kept exactly as it is, formatted `@`.
    Text,
    /// A date read in this part order.
    Date(DateOrder),
    /// Do not import the column.
    Skip,
}

impl ColFormat {
    /// `general`, `text`, `skip`, `date` (MDY) or `date:dmy`.
    pub fn parse(s: &str) -> Option<ColFormat> {
        let s = s.trim().to_ascii_lowercase();
        Some(match s.as_str() {
            "general" | "g" => ColFormat::General,
            "text" | "t" => ColFormat::Text,
            "skip" | "s" | "do not import" => ColFormat::Skip,
            "date" | "d" => ColFormat::Date(DateOrder::Mdy),
            _ => {
                let order = s
                    .strip_prefix("date:")
                    .or_else(|| s.strip_prefix("date "))?;
                ColFormat::Date(DateOrder::parse(order)?)
            }
        })
    }

    /// The inverse of [`ColFormat::parse`].
    pub fn name(self) -> String {
        match self {
            ColFormat::General => "general".into(),
            ColFormat::Text => "text".into(),
            ColFormat::Skip => "skip".into(),
            ColFormat::Date(o) => format!("date:{}", o.name().to_ascii_lowercase()),
        }
    }
}

/// The delimiter checkboxes of the wizard's step 2.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Delimiters {
    pub tab: bool,
    pub semicolon: bool,
    pub comma: bool,
    pub space: bool,
    pub other: Option<char>,
}

impl Delimiters {
    /// Exactly one delimiter, ticked in its box when it has one.
    pub fn only(c: char) -> Delimiters {
        let mut d = Delimiters::default();
        match c {
            '\t' => d.tab = true,
            ';' => d.semicolon = true,
            ',' => d.comma = true,
            ' ' => d.space = true,
            other => d.other = Some(other),
        }
        d
    }

    pub fn contains(&self, c: char) -> bool {
        (self.tab && c == '\t')
            || (self.semicolon && c == ';')
            || (self.comma && c == ',')
            || (self.space && c == ' ')
            || self.other == Some(c)
    }
}

/// Delimited or fixed width (the wizard's step 1 choice).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SplitKind {
    Delimited {
        delims: Delimiters,
        /// Treat consecutive delimiters as one.
        consecutive: bool,
    },
    /// Field breaks at these character positions (the break lines).
    Fixed { breaks: Vec<usize> },
}

/// Everything the Text Import Wizard and Text to Columns ask for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextParse {
    pub kind: SplitKind,
    /// The text qualifier (`"`, `'` or none). Delimited only.
    pub qualifier: Option<char>,
    /// The first record imported, 1-based ("Start import at row").
    pub start_row: usize,
    /// Per-column formats; a column past the end is General.
    pub columns: Vec<ColFormat>,
    /// Advanced: the decimal separator.
    pub decimal: char,
    /// Advanced: the thousands separator.
    pub thousands: char,
    /// Advanced: a trailing minus (`5-`) makes a negative number.
    pub trailing_minus: bool,
}

impl Default for TextParse {
    /// The wizard's defaults: delimited by Tab, `"` qualifier, en-US
    /// separators, trailing minus on.
    fn default() -> TextParse {
        TextParse {
            kind: SplitKind::Delimited {
                delims: Delimiters::only('\t'),
                consecutive: false,
            },
            qualifier: Some('"'),
            start_row: 1,
            columns: Vec::new(),
            decimal: '.',
            thousands: ',',
            trailing_minus: true,
        }
    }
}

impl TextParse {
    /// How opening a `.csv` reads it: one delimiter, `"`, every field
    /// converted as typed entry (so no trailing minus).
    pub fn csv(delim: char) -> TextParse {
        TextParse {
            kind: SplitKind::Delimited {
                delims: Delimiters::only(delim),
                consecutive: false,
            },
            trailing_minus: false,
            ..TextParse::default()
        }
    }

    /// The format of field `i`.
    pub fn column(&self, i: usize) -> ColFormat {
        self.columns.get(i).copied().unwrap_or_default()
    }
}

/// File › Options › Data › Automatic Data Conversion. All on is Excel's
/// default (and converts as typed entry does); each switch turned off keeps
/// its kind of field as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoConvert {
    /// Remove leading zeros and convert to number (`007` → 7).
    pub remove_leading_zeros: bool,
    /// Keep the first 15 digits of long numbers (else they stay text).
    pub keep_15_digits: bool,
    /// Convert digits around an `E` to scientific notation (`1E5`).
    pub e_notation: bool,
    /// Convert continuous letters and numbers to a date (`1/2`, `Mar1`).
    pub dates: bool,
}

impl Default for AutoConvert {
    fn default() -> AutoConvert {
        AutoConvert {
            remove_leading_zeros: true,
            keep_15_digits: true,
            e_notation: true,
            dates: true,
        }
    }
}

impl AutoConvert {
    /// All four switches off.
    #[cfg(test)]
    pub fn off() -> AutoConvert {
        AutoConvert {
            remove_leading_zeros: false,
            keep_15_digits: false,
            e_notation: false,
            dates: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// The wizard's File origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    /// A byte-order mark decides; else UTF-8 when the bytes are valid UTF-8,
    /// else Windows-1252.
    #[default]
    Auto,
    Utf8,
    Utf16Le,
    Windows1252,
}

impl Origin {
    pub fn parse(s: &str) -> Option<Origin> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Origin::Auto,
            "utf-8" | "utf8" | "65001" => Origin::Utf8,
            "utf-16" | "utf-16le" | "utf16" | "unicode" | "1200" => Origin::Utf16Le,
            "windows-1252" | "1252" | "cp1252" | "ansi" => Origin::Windows1252,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Origin::Auto => "auto",
            Origin::Utf8 => "utf-8",
            Origin::Utf16Le => "utf-16le",
            Origin::Windows1252 => "windows-1252",
        }
    }
}

/// Windows-1252's 0x80–0x9F; the five holes map to the C1 control of the
/// same number, as Windows does.
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

fn cp1252_char(b: u8) -> char {
    match b {
        0x80..=0x9F => CP1252_HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

fn cp1252_byte(c: char) -> Option<u8> {
    let u = c as u32;
    if u < 0x80 || (0xA0..=0xFF).contains(&u) {
        return Some(u as u8);
    }
    CP1252_HIGH
        .iter()
        .position(|&h| h == c)
        .map(|i| 0x80 + i as u8)
}

fn utf16le(bytes: &[u8]) -> String {
    let units = bytes
        .chunks(2)
        .map(|p| u16::from_le_bytes([p[0], p.get(1).copied().unwrap_or(0)]));
    char::decode_utf16(units)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

/// Bytes read from a text file, as text; a byte-order mark is dropped.
pub fn decode(bytes: &[u8], origin: Origin) -> String {
    let utf8_bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    let utf16_bom = bytes.starts_with(&[0xFF, 0xFE]);
    match origin {
        Origin::Auto if utf8_bom => String::from_utf8_lossy(&bytes[3..]).into_owned(),
        Origin::Auto if utf16_bom => utf16le(&bytes[2..]),
        Origin::Auto => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_string(),
            Err(_) => bytes.iter().map(|&b| cp1252_char(b)).collect(),
        },
        Origin::Utf8 => {
            let body = if utf8_bom { &bytes[3..] } else { bytes };
            String::from_utf8_lossy(body).into_owned()
        }
        Origin::Utf16Le => utf16le(if utf16_bom { &bytes[2..] } else { bytes }),
        Origin::Windows1252 => bytes.iter().map(|&b| cp1252_char(b)).collect(),
    }
}

/// A CSV's `sep=<c>` first line: the delimiter it names and the text after
/// it. Any other first line leaves the text as it is.
pub fn csv_directive(text: &str) -> (Option<char>, &str) {
    let (line, rest) = match text.find('\n') {
        Some(i) => (&text[..i], &text[i + 1..]),
        None => (text, ""),
    };
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.len() > 4 && line.is_char_boundary(4) && line[..4].eq_ignore_ascii_case("sep=") {
        let mut chars = line[4..].chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return (Some(c), rest);
        }
    }
    (None, text)
}

// ---------------------------------------------------------------------------
// Splitting
// ---------------------------------------------------------------------------

/// Split `text` into records of fields under `opts`, dropping the records
/// before `opts.start_row`. Records end at CR LF, LF or CR outside a
/// qualified field; a line break inside one is kept as LF.
pub fn split_text(text: &str, opts: &TextParse) -> Vec<Vec<String>> {
    let mut records = match &opts.kind {
        SplitKind::Delimited {
            delims,
            consecutive,
        } => split_delimited(text, delims, *consecutive, opts.qualifier, true),
        SplitKind::Fixed { breaks } => lines(text)
            .into_iter()
            .map(|l| split_fixed(l, breaks))
            .collect(),
    };
    let skip = opts.start_row.saturating_sub(1).min(records.len());
    records.drain(..skip);
    records
}

/// Split one cell's text into fields (Text to Columns): a line break is an
/// ordinary character here.
pub fn split_value(text: &str, opts: &TextParse) -> Vec<String> {
    match &opts.kind {
        SplitKind::Delimited {
            delims,
            consecutive,
        } => split_delimited(text, delims, *consecutive, opts.qualifier, false)
            .into_iter()
            .next()
            .unwrap_or_default(),
        SplitKind::Fixed { breaks } => split_fixed(text, breaks),
    }
}

fn lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\n' => {
                out.push(&text[start..i]);
                start = i + 1;
            }
            b'\r' => {
                out.push(&text[start..i]);
                if b.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// The fields of one fixed-width line: the text between break positions,
/// with the padding spaces trimmed.
fn split_fixed(line: &str, breaks: &[usize]) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut cuts: Vec<usize> = breaks.iter().copied().filter(|&b| b > 0).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::with_capacity(cuts.len() + 1);
    let mut from = 0;
    for end in cuts.into_iter().chain(std::iter::once(usize::MAX)) {
        let a = from.min(chars.len());
        let b = end.min(chars.len());
        let field: String = chars[a..b].iter().collect();
        out.push(field.trim_matches(' ').to_string());
        from = end;
    }
    out
}

fn split_delimited(
    text: &str,
    delims: &Delimiters,
    consecutive: bool,
    qualifier: Option<char>,
    multi: bool,
) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut at_start = true;
    let mut in_q = false;
    // Something (a delimiter or a qualifier) was read since the last record
    // ended, so an empty trailing field still counts.
    let mut pending = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_q {
            if Some(ch) == qualifier {
                if chars.peek() == Some(&ch) {
                    chars.next();
                    field.push(ch);
                } else {
                    in_q = false;
                }
            } else if ch == '\r' {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                field.push('\n');
            } else {
                field.push(ch);
            }
            continue;
        }
        if at_start && qualifier == Some(ch) {
            in_q = true;
            at_start = false;
            pending = true;
            continue;
        }
        if delims.contains(ch) {
            record.push(std::mem::take(&mut field));
            at_start = true;
            pending = true;
            if consecutive {
                while chars.peek().is_some_and(|&c| delims.contains(c)) {
                    chars.next();
                }
            }
            continue;
        }
        if multi && (ch == '\n' || ch == '\r') {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            record.push(std::mem::take(&mut field));
            records.push(std::mem::take(&mut record));
            at_start = true;
            pending = false;
            continue;
        }
        field.push(ch);
        at_start = false;
        pending = true;
    }
    if pending || !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    } else if !multi {
        records.push(vec![String::new()]);
    }
    records
}

// ---------------------------------------------------------------------------
// Converting
// ---------------------------------------------------------------------------

fn text_entry(field: &str) -> Entry {
    Entry {
        cell: Cell::text(field),
        format: None,
        quote_prefix: false,
        wrap: field.contains('\n'),
    }
}

/// A converted field: the entry and whether its format must replace the
/// destination's (a Text or Date column), rather than only fill a General one.
#[derive(Clone, Debug, PartialEq)]
pub struct Converted {
    pub entry: Entry,
    pub force_format: bool,
}

/// One field as a cell under `format`; `None` for a skipped column.
pub fn convert_field(
    field: &str,
    format: ColFormat,
    opts: &TextParse,
    auto: &AutoConvert,
    ctx: &EntryCtx,
) -> Option<Converted> {
    let plain = |entry| Converted {
        entry,
        force_format: false,
    };
    match format {
        ColFormat::Skip => None,
        _ if field.is_empty() => Some(plain(Entry {
            cell: Cell::default(),
            format: None,
            quote_prefix: false,
            wrap: false,
        })),
        ColFormat::Text => Some(Converted {
            entry: Entry {
                format: Some("@"),
                ..text_entry(field)
            },
            force_format: true,
        }),
        // A Date column reads its own order only: a field that is not a date
        // in that order stays text, never re-read as another order.
        ColFormat::Date(order) => match parse_ordered_date_time(field.trim(), order, ctx) {
            Some((serial, format)) => Some(Converted {
                entry: Entry {
                    cell: Cell::number(serial),
                    format: Some(format),
                    quote_prefix: false,
                    wrap: false,
                },
                force_format: true,
            }),
            None => Some(plain(text_entry(field))),
        },
        ColFormat::General => Some(plain(general(field, opts, auto, ctx))),
    }
}

fn general(field: &str, opts: &TextParse, auto: &AutoConvert, ctx: &EntryCtx) -> Entry {
    let t = field.trim();
    // A leading apostrophe is data in a text file, not the typed-text marker.
    if field.starts_with('\'') {
        return text_entry(field);
    }
    if (!auto.remove_leading_zeros && leading_zero_number(t))
        || (!auto.e_notation && e_notation(t))
        || (!auto.keep_15_digits && long_number(t))
    {
        return text_entry(field);
    }
    // A number written with the Advanced separators is a number only if it
    // reads as one there (groups of three included): `03.04.2024` under a
    // `.` thousands separator stays text rather than becoming 3042024.
    if let Some(n) = normalize_number(t, opts) {
        return match entry::parse_entry(&n, &Xf::default(), ctx) {
            Ok(e) if e.cell.formula.is_none() && matches!(e.cell.value, CellValue::Number(_)) => e,
            _ => text_entry(field),
        };
    }
    let e = match entry::parse_entry(field, &Xf::default(), ctx) {
        Ok(e) => e,
        Err(_) => return text_entry(field),
    };
    if !auto.dates && e.format.is_some_and(is_date_code) {
        return text_entry(field);
    }
    e
}

fn is_date_code(code: &str) -> bool {
    matches!(classify_format_code(code), NumFmt::Date | NumFmt::DateTime)
}

fn unsigned(t: &str) -> &str {
    t.strip_prefix(['-', '+']).unwrap_or(t)
}

fn plain_digits(t: &str) -> bool {
    !t.is_empty()
        && t.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && t.matches('.').count() <= 1
}

/// `007`, `-0012.5`: a number written with a leading zero.
fn leading_zero_number(t: &str) -> bool {
    let s = unsigned(t);
    let b = s.as_bytes();
    b.len() >= 2 && b[0] == b'0' && b[1].is_ascii_digit() && plain_digits(s)
}

/// `1E5`, `2.5e-3`.
fn e_notation(t: &str) -> bool {
    let s = unsigned(t);
    let Some(i) = s.find(['e', 'E']) else {
        return false;
    };
    let (m, e) = (&s[..i], &s[i + 1..]);
    let e = e.strip_prefix(['+', '-']).unwrap_or(e);
    plain_digits(m)
        && m.bytes().any(|b| b.is_ascii_digit())
        && !e.is_empty()
        && e.bytes().all(|b| b.is_ascii_digit())
}

/// A plain number with more than 15 significant digits.
fn long_number(t: &str) -> bool {
    let s = unsigned(t);
    plain_digits(s)
        && s.bytes()
            .filter(u8::is_ascii_digit)
            .skip_while(|&b| b == b'0')
            .count()
            > 15
}

/// A number written with the Advanced separators and trailing minus,
/// rewritten the way en-US entry reads it (the decimal separator becomes `.`
/// and the thousands separator `,`, so entry's own groups-of-three rule
/// decides); `None` when nothing changes or the field is not number-shaped.
fn normalize_number(t: &str, opts: &TextParse) -> Option<String> {
    let mut core = t;
    let mut negative = false;
    if opts.trailing_minus && core.len() > 1 && core.ends_with('-') {
        let rest = core[..core.len() - 1].trim_end();
        if rest.bytes().any(|b| b.is_ascii_digit()) && !rest.starts_with(['-', '(']) {
            core = rest;
            negative = true;
        }
    }
    let custom = (opts.decimal, opts.thousands) != ('.', ',');
    if !negative && !custom {
        return None;
    }
    let allowed = |c: char| {
        c.is_ascii_digit()
            || c == opts.decimal
            || c == opts.thousands
            || matches!(c, '%' | '$' | 'e' | 'E' | '+' | '-')
    };
    if !core.chars().all(allowed) || !core.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    for c in core.chars() {
        out.push(if c == opts.decimal {
            '.'
        } else if c == opts.thousands {
            ','
        } else {
            c
        });
    }
    Some(out)
}

/// [`parse_ordered_date`], optionally followed by a space and a time
/// (`03/04/2024 10:30`): the serial and the format Excel gives it.
pub fn parse_ordered_date_time(
    t: &str,
    order: DateOrder,
    ctx: &EntryCtx,
) -> Option<(f64, &'static str)> {
    if let Some(day) = parse_ordered_date(t, order, ctx) {
        return Some((day, "m/d/yyyy"));
    }
    // A date, a space, then a time: try every split, rightmost first.
    for (i, _) in t.match_indices(' ').collect::<Vec<_>>().into_iter().rev() {
        let (d, tm) = (t[..i].trim_end(), t[i + 1..].trim_start());
        if let (Some(day), Some((frac, _))) =
            (parse_ordered_date(d, order, ctx), entry::parse_time(tm))
        {
            if frac < 1.0 {
                return Some((day + frac, "m/d/yyyy h:mm"));
            }
        }
    }
    None
}

/// A date whose parts come in `order`: `03/04/2024`, `3.4.24`, `03042024`,
/// `3-Apr-2024` or, without the year, `03/04` in the current year.
pub fn parse_ordered_date(t: &str, order: DateOrder, ctx: &EntryCtx) -> Option<f64> {
    let mut parts: Vec<&str> = t
        .split(['/', '-', '.', ' ', ','])
        .filter(|p| !p.is_empty())
        .collect();
    let mut keys: Vec<char> = order.name().chars().collect();
    if parts.len() == 1 && t.bytes().all(|b| b.is_ascii_digit()) && matches!(t.len(), 6 | 8) {
        // Digits only: two for the day and month, the rest for the year.
        let ylen = t.len() - 4;
        let mut at = 0;
        let mut cut = Vec::new();
        for k in &keys {
            let n = if *k == 'Y' { ylen } else { 2 };
            cut.push(&t[at..at + n]);
            at += n;
        }
        parts = cut;
    }
    if parts.len() == 2 {
        keys.retain(|&k| k != 'Y');
    }
    if parts.len() != keys.len() {
        return None;
    }
    let (mut y, mut m, mut d) = (None, None, None);
    for (k, p) in keys.iter().zip(&parts) {
        match k {
            'Y' => y = Some(entry::year(p)?),
            'M' => {
                m = Some(match entry::num(p) {
                    Some(n) => n as u32,
                    None => entry::month_name(p)?,
                })
            }
            _ => d = Some(entry::num(p)? as u32),
        }
    }
    let y = match y {
        Some(y) => y,
        None => serial_to_parts(ctx.today?.floor(), false)?.year,
    };
    let (m, d) = (m?, d?);
    let first_year = if ctx.date1904 { 1904 } else { 1900 };
    if !(first_year..=9999).contains(&y)
        || !(1..=12).contains(&m)
        || d < 1
        || d > crate::formula::days_in_month(y, m)
    {
        return None;
    }
    let serial = parts_to_serial(y, m, d, 0, ctx.date1904);
    (serial >= 0.0).then_some(serial)
}

/// Write `records` into `sheet` with its top-left field at (`top`, `left`),
/// converting each field under its column's format. Skipped columns take no
/// cell; an empty field clears its cell (keeping the style). Returns the
/// (rows, columns) written.
#[allow(clippy::too_many_arguments)]
pub fn import_records(
    sheet: &mut Sheet,
    styles: &mut Styles,
    top: u32,
    left: u32,
    records: &[Vec<String>],
    opts: &TextParse,
    auto: &AutoConvert,
    ctx: &EntryCtx,
) -> (u32, u32) {
    let mut width = 0;
    for (ri, rec) in records.iter().enumerate() {
        let r = top + ri as u32;
        let mut oc = 0u32;
        for (fi, field) in rec.iter().enumerate() {
            let Some(conv) = convert_field(field, opts.column(fi), opts, auto, ctx) else {
                continue;
            };
            let c = left + oc;
            oc += 1;
            put(sheet, styles, r, c, conv);
        }
        width = width.max(oc);
    }
    (records.len() as u32, width)
}

/// Store a converted field at (`r`, `c`) over whatever style the cell had.
pub fn put(sheet: &mut Sheet, styles: &mut Styles, r: u32, c: u32, conv: Converted) {
    let base = sheet.cell(r, c).map_or(0, |cell| cell.style);
    let old = styles.xf(base);
    let mut xf = entry::entry_xf(&old, &conv.entry);
    if conv.force_format {
        xf.set_code(conv.entry.format.map(str::to_string));
    }
    let style = if xf == old { base } else { styles.intern(xf) };
    sheet.set_cell(
        r,
        c,
        Cell {
            style,
            ..conv.entry.cell
        },
    );
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// A text file's encoding on save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// CSV UTF-8: `EF BB BF`, then UTF-8.
    Utf8Bom,
    /// CSV, Text (Tab delimited), Formatted Text: a character outside the
    /// code page becomes `?`.
    Windows1252,
    /// Unicode Text: `FF FE`, then UTF-16 LE.
    Utf16LeBom,
}

pub fn encode(text: &str, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf8Bom => {
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend_from_slice(text.as_bytes());
            out
        }
        Encoding::Windows1252 => text
            .chars()
            .map(|c| cp1252_byte(c).unwrap_or(b'?'))
            .collect(),
        Encoding::Utf16LeBom => {
            let mut out = vec![0xFF, 0xFE];
            for u in text.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
            out
        }
    }
}

/// A cell's text as a text file holds it: display-formatted, with any line
/// break inside it a bare LF.
fn shown(cell: &Cell, styles: &Styles, date1904: bool) -> String {
    let text = format_with(&styles.xf(cell.style), &cell.value, date1904);
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    }
}

/// A sheet as delimited text, as Excel writes it: display values, a field
/// holding the delimiter, a quote or a line break in double quotes, CR LF
/// after every record (the used range's full width), LF inside a field.
pub fn sheet_text(sheet: &Sheet, styles: &Styles, date1904: bool, delim: char) -> String {
    let (rows, cols) = sheet.used_size();
    let mut out = String::new();
    for r in 0..rows {
        for c in 0..cols {
            if c > 0 {
                out.push(delim);
            }
            if let Some(cell) = sheet.cell(r, c) {
                let text = shown(cell, styles, date1904);
                if text.contains([delim, '"', '\n']) {
                    out.push('"');
                    out.push_str(&text.replace('"', "\"\""));
                    out.push('"');
                } else {
                    out.push_str(&text);
                }
            }
        }
        out.push_str("\r\n");
    }
    out
}

/// Formatted Text (Space delimited): each column padded to its width in
/// characters, numbers right-aligned and text left-aligned. Text longer than
/// its column is clipped; a number never is.
pub fn sheet_prn(sheet: &Sheet, styles: &Styles, date1904: bool) -> String {
    let (rows, cols) = sheet.used_size();
    let mut out = String::new();
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..cols {
            let width = (sheet.col_width(c).round() as usize).max(1);
            let (text, right) = match sheet.cell(r, c) {
                Some(cell) => (
                    shown(cell, styles, date1904).replace('\n', " "),
                    matches!(cell.value, CellValue::Number(_)),
                ),
                None => (String::new(), false),
            };
            // A number is written whole, even past its column: clipping it
            // would write another value. Text is clipped to the column.
            let text: String = if right {
                text
            } else {
                text.chars().take(width).collect()
            };
            let pad = " ".repeat(width.saturating_sub(text.chars().count()));
            if right {
                line.push_str(&pad);
                line.push_str(&text);
            } else {
                line.push_str(&text);
                line.push_str(&pad);
            }
        }
        out.push_str(line.trim_end_matches(' '));
        out.push_str("\r\n");
    }
    out
}

/// Web Page: the `.htm` and the files of its `<stem>_files` folder (name,
/// contents), for one sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct WebPage {
    pub htm: String,
    pub files: Vec<(String, String)>,
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("<br>"),
            _ => out.push(c),
        }
    }
    out
}

/// A sheet as Excel's Web Page saved as `file_name` (`out.htm`, `out.html`):
/// the page, linking `<stem>_files/filelist.xml` and `stylesheet.css`.
pub fn web_page(sheet: &Sheet, styles: &Styles, date1904: bool, file_name: &str) -> WebPage {
    let stem = match file_name.rfind('.') {
        Some(i) if i > 0 => &file_name[..i],
        _ => file_name,
    };
    let folder = format!("{stem}_files");
    let (rows, cols) = sheet.used_size();
    let mut table = String::new();
    for c in 0..cols {
        let px = (sheet.col_width(c) * 7.0 + 5.0).round() as u32;
        table.push_str(&format!(" <col width={px}>\r\n"));
    }
    for r in 0..rows {
        table.push_str(" <tr>\r\n");
        for c in 0..cols {
            match sheet.cell(r, c) {
                Some(cell) => {
                    let class = if matches!(cell.value, CellValue::Number(_)) {
                        " align=right"
                    } else {
                        ""
                    };
                    table.push_str(&format!(
                        "  <td{class}>{}</td>\r\n",
                        html_escape(&shown(cell, styles, date1904))
                    ));
                }
                None => table.push_str("  <td></td>\r\n"),
            }
        }
        table.push_str(" </tr>\r\n");
    }
    let title = html_escape(&sheet.name);
    let htm = format!(
        "<html xmlns:o=\"urn:schemas-microsoft-com:office:office\"\r\n\
         xmlns:x=\"urn:schemas-microsoft-com:office:excel\"\r\n\
         xmlns=\"http://www.w3.org/TR/REC-html40\">\r\n\
         <head>\r\n\
         <meta http-equiv=Content-Type content=\"text/html; charset=utf-8\">\r\n\
         <meta name=ProgId content=Excel.Sheet>\r\n\
         <link rel=File-List href=\"{folder}/filelist.xml\">\r\n\
         <link rel=Stylesheet href=\"{folder}/stylesheet.css\">\r\n\
         <title>{title}</title>\r\n\
         </head>\r\n\
         <body>\r\n\
         <table border=0 cellpadding=0 cellspacing=0 style='border-collapse:collapse'>\r\n\
         {table}</table>\r\n\
         </body>\r\n\
         </html>\r\n"
    );
    let filelist = format!(
        "<xml xmlns:o=\"urn:schemas-microsoft-com:office:office\">\r\n \
         <o:MainFile HRef=\"../{file_name}\"/>\r\n \
         <o:File HRef=\"stylesheet.css\"/>\r\n \
         <o:File HRef=\"filelist.xml\"/>\r\n\
         </xml>\r\n"
    );
    let css = "tr\r\n\t{mso-height-source:auto;}\r\n\
               col\r\n\t{mso-width-source:auto;}\r\n\
               td\r\n\t{padding-top:1px;\r\n\tpadding-right:1px;\r\n\tpadding-left:1px;\r\n\
               \tfont-size:11.0pt;\r\n\tfont-family:Calibri, sans-serif;\r\n\
               \tvertical-align:bottom;\r\n\twhite-space:nowrap;}\r\n"
        .to_string();
    WebPage {
        htm,
        files: vec![
            ("filelist.xml".to_string(), filelist),
            ("stylesheet.css".to_string(), css),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-09-30 as a 1900-system serial.
    const TODAY: f64 = 45_565.0;

    fn ctx() -> EntryCtx {
        EntryCtx {
            date1904: false,
            today: Some(TODAY),
        }
    }

    fn serial(y: i64, m: u32, d: u32) -> f64 {
        parts_to_serial(y, m, d, 0, false)
    }

    fn delimited(delims: Delimiters, consecutive: bool) -> TextParse {
        TextParse {
            kind: SplitKind::Delimited {
                delims,
                consecutive,
            },
            ..TextParse::default()
        }
    }

    fn comma() -> TextParse {
        delimited(Delimiters::only(','), false)
    }

    fn fields(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_qualifier_keeps_delimiters_and_doubled_quotes_inside_a_field() {
        assert_eq!(
            split_value("Pen,4,\"Blue, fine\",0012", &comma()),
            fields(&["Pen", "4", "Blue, fine", "0012"])
        );
        assert_eq!(
            split_value("\"say \"\"hi\"\"\",x", &comma()),
            fields(&["say \"hi\"", "x"])
        );
        // A qualifier opens a field only at its start.
        assert_eq!(split_value("a\"b,c", &comma()), fields(&["a\"b", "c"]));
        // Without a qualifier the quotes are data.
        let none = TextParse {
            qualifier: None,
            ..comma()
        };
        assert_eq!(split_value("\"a,b\"", &none), fields(&["\"a", "b\""]));
    }

    #[test]
    fn empty_fields_are_kept_unless_consecutive_delimiters_are_one() {
        assert_eq!(
            split_value("Ink,,Red,7", &comma()),
            fields(&["Ink", "", "Red", "7"])
        );
        let one = delimited(Delimiters::only(','), true);
        assert_eq!(
            split_value("Ink,,Red,7", &one),
            fields(&["Ink", "Red", "7"])
        );
        assert_eq!(split_value("a,", &comma()), fields(&["a", ""]));
        assert_eq!(split_value("", &comma()), fields(&[""]));
    }

    #[test]
    fn several_delimiters_split_at_once() {
        let all = Delimiters {
            tab: true,
            semicolon: true,
            comma: true,
            space: true,
            other: Some('|'),
        };
        assert_eq!(
            split_value("a\tb;c,d e|f", &delimited(all, false)),
            fields(&["a", "b", "c", "d", "e", "f"])
        );
        // Space is only a delimiter when ticked: nothing is trimmed.
        assert_eq!(split_value(" a , b", &comma()), fields(&[" a ", " b"]));
    }

    #[test]
    fn fixed_width_cuts_at_the_break_positions() {
        let opts = TextParse {
            kind: SplitKind::Fixed { breaks: vec![5, 3] },
            ..TextParse::default()
        };
        assert_eq!(
            split_value("abcdefghij", &opts),
            fields(&["abc", "de", "fghij"])
        );
        assert_eq!(split_value("ab", &opts), fields(&["ab", "", ""]));
        assert_eq!(
            split_text("12 xy 7\r\n3  z  8\r\n", &opts),
            vec![fields(&["12", "xy", "7"]), fields(&["3", "z", "8"])]
        );
    }

    #[test]
    fn records_end_at_any_line_break_outside_a_qualified_field() {
        let text = "a,b\r\nc,\"x\r\ny\"\nd,e\rf,g";
        assert_eq!(
            split_text(text, &comma()),
            vec![
                fields(&["a", "b"]),
                fields(&["c", "x\ny"]),
                fields(&["d", "e"]),
                fields(&["f", "g"]),
            ]
        );
        // A trailing newline adds no record; an empty line is one.
        assert_eq!(split_text("a\n\nb\n", &comma()).len(), 3);
        let from3 = TextParse {
            start_row: 3,
            ..comma()
        };
        assert_eq!(
            split_text("h\nx\n1\n2\n", &from3),
            vec![fields(&["1"]), fields(&["2"])]
        );
    }

    #[test]
    fn decode_honours_the_byte_order_mark_and_falls_back_to_1252() {
        assert_eq!(decode(b"\xEF\xBB\xBFZ\xC3\xBCrich", Origin::Auto), "Zürich");
        assert_eq!(decode(b"\xFF\xFEa\x00\xFC\x00", Origin::Auto), "aü");
        assert_eq!(decode(b"Z\xFCrich \x80", Origin::Auto), "Zürich €");
        assert_eq!(decode(b"\xC3\xBC", Origin::Windows1252), "Ã¼");
        assert_eq!(decode(b"a\x00b\x00", Origin::Utf16Le), "ab");
    }

    #[test]
    fn a_sep_first_line_sets_the_delimiter_and_is_dropped() {
        assert_eq!(csv_directive("sep=;\r\na;b\r\n"), (Some(';'), "a;b\r\n"));
        assert_eq!(csv_directive("SEP=|\na|b"), (Some('|'), "a|b"));
        assert_eq!(csv_directive("sep=\na"), (None, "sep=\na"));
        assert_eq!(csv_directive("sep=;;\na"), (None, "sep=;;\na"));
        assert_eq!(csv_directive("a,b\n"), (None, "a,b\n"));
    }

    fn general_value(field: &str, auto: &AutoConvert) -> Entry {
        convert_field(
            field,
            ColFormat::General,
            &TextParse::csv(','),
            auto,
            &ctx(),
        )
        .unwrap()
        .entry
    }

    #[test]
    fn csv_fields_convert_as_typed_entry() {
        let on = AutoConvert::default();
        let v = |f: &str| general_value(f, &on).cell;
        assert_eq!(v("007").value, CellValue::Number(7.0));
        let jan2 = general_value("1/2", &on);
        assert_eq!(jan2.cell.value, CellValue::Number(serial(2024, 1, 2)));
        assert!(jan2.format.is_some_and(is_date_code));
        let pct = general_value("12%", &on);
        assert_eq!(pct.cell.value, CellValue::Number(0.12));
        assert_eq!(pct.format, Some("0%"));
        assert_eq!(v("TRUE").value, CellValue::Bool(true));
        assert_eq!(v("1E5").value, CellValue::Number(100_000.0));
        assert_eq!(
            v("1234567890123456789").value,
            CellValue::Number(1.23456789012346e18)
        );
        assert_eq!(v("=1+1").formula.as_deref(), Some("1+1"));
        assert_eq!(v("apple").value, CellValue::Text("apple".into()));
        assert_eq!(v("a,b").value, CellValue::Text("a,b".into()));
        // An apostrophe is kept as data.
        assert_eq!(v("'007").value, CellValue::Text("'007".into()));
    }

    #[test]
    fn with_automatic_conversion_off_those_fields_stay_text() {
        let off = AutoConvert::off();
        for f in ["007", "1/2", "1E5", "1234567890123456789"] {
            assert_eq!(
                general_value(f, &off).cell.value,
                CellValue::Text(f.into()),
                "{f}"
            );
        }
        // Everything else still converts.
        assert_eq!(general_value("7", &off).cell.value, CellValue::Number(7.0));
        assert_eq!(
            general_value("0.5", &off).cell.value,
            CellValue::Number(0.5)
        );
        assert_eq!(
            general_value("123456789012345", &off).cell.value,
            CellValue::Number(123_456_789_012_345.0)
        );
        // One switch at a time.
        let only_zeros = AutoConvert {
            remove_leading_zeros: false,
            ..AutoConvert::default()
        };
        assert_eq!(
            general_value("007", &only_zeros).cell.value,
            CellValue::Text("007".into())
        );
        assert_eq!(
            general_value("1E5", &only_zeros).cell.value,
            CellValue::Number(100_000.0)
        );
    }

    #[test]
    fn the_wizard_example_converts_per_column() {
        let opts = TextParse {
            columns: vec![
                ColFormat::Text,
                ColFormat::Date(DateOrder::Dmy),
                ColFormat::General,
                ColFormat::Skip,
            ],
            decimal: ',',
            thousands: '.',
            trailing_minus: true,
            ..TextParse::default()
        };
        let recs = split_text("02134\t03/04/2024\t1.234,5-\tx\r\n", &opts);
        let mut sheet = Sheet::default();
        let mut styles = Styles::default();
        let (rows, cols) = import_records(
            &mut sheet,
            &mut styles,
            0,
            0,
            &recs,
            &opts,
            &AutoConvert::default(),
            &ctx(),
        );
        assert_eq!((rows, cols), (1, 3));
        let a = sheet.cell(0, 0).unwrap();
        assert_eq!(a.value, CellValue::Text("02134".into()));
        assert_eq!(styles.xf(a.style).code.as_deref(), Some("@"));
        let b = sheet.cell(0, 1).unwrap();
        assert_eq!(b.value, CellValue::Number(serial(2024, 4, 3)));
        assert_eq!(styles.xf(b.style).numfmt, NumFmt::Date);
        assert_eq!(sheet.cell(0, 2).unwrap().value, CellValue::Number(-1234.5));
        assert!(sheet.cell(0, 3).is_none());
    }

    #[test]
    fn ordered_dates_read_their_parts_in_order() {
        let c = ctx();
        let d = |t: &str, o| parse_ordered_date(t, o, &c);
        assert_eq!(d("03/04/2024", DateOrder::Mdy), Some(serial(2024, 3, 4)));
        assert_eq!(d("03/04/2024", DateOrder::Dmy), Some(serial(2024, 4, 3)));
        assert_eq!(d("2024-04-03", DateOrder::Ymd), Some(serial(2024, 4, 3)));
        assert_eq!(d("3.4.24", DateOrder::Dmy), Some(serial(2024, 4, 3)));
        assert_eq!(d("03042024", DateOrder::Dmy), Some(serial(2024, 4, 3)));
        assert_eq!(d("3-Apr-2024", DateOrder::Dmy), Some(serial(2024, 4, 3)));
        assert_eq!(d("03/04", DateOrder::Dmy), Some(serial(2024, 4, 3)));
        assert_eq!(d("31/02/2024", DateOrder::Dmy), None);
        assert_eq!(d("apple", DateOrder::Dmy), None);
    }

    #[test]
    fn column_formats_parse_and_print() {
        for f in [
            ColFormat::General,
            ColFormat::Text,
            ColFormat::Skip,
            ColFormat::Date(DateOrder::Ydm),
        ] {
            assert_eq!(ColFormat::parse(&f.name()), Some(f));
        }
        assert_eq!(
            ColFormat::parse("date"),
            Some(ColFormat::Date(DateOrder::Mdy))
        );
        assert_eq!(ColFormat::parse("nonsense"), None);
    }

    fn sheet_of(rows: &[&[&str]]) -> Sheet {
        let mut s = Sheet::default();
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                if !v.is_empty() {
                    s.set_cell(r as u32, c as u32, Cell::text(v));
                }
            }
        }
        s
    }

    #[test]
    fn csv_is_written_with_cr_lf_records_and_lf_inside_a_field() {
        // A cell loaded with a CR LF (openpyxl on Windows writes one into
        // the XML) is still written with a bare LF.
        let s = sheet_of(&[&["Name", "Note"], &["Zürich", "line1\r\nline2"]]);
        let text = sheet_text(&s, &Styles::default(), false, ',');
        assert_eq!(text, "Name,Note\r\nZürich,\"line1\nline2\"\r\n");
        let mut want = vec![0xEF, 0xBB, 0xBF];
        want.extend_from_slice("Name,Note\r\nZürich,\"line1\nline2\"\r\n".as_bytes());
        assert_eq!(encode(&text, Encoding::Utf8Bom), want);
        // Quotes, delimiters, a lone CR, and padding to the used width.
        let s = sheet_of(&[&["a\"b", "c,d", "e\rf"], &["x", "", ""]]);
        assert_eq!(
            sheet_text(&s, &Styles::default(), false, ','),
            "\"a\"\"b\",\"c,d\",\"e\nf\"\r\nx,,\r\n"
        );
    }

    #[test]
    fn tab_text_quotes_only_what_needs_it() {
        let s = sheet_of(&[&["a,b", "c\td"]]);
        assert_eq!(
            sheet_text(&s, &Styles::default(), false, '\t'),
            "a,b\t\"c\td\"\r\n"
        );
    }

    #[test]
    fn encodings_write_their_marks_and_code_pages() {
        assert_eq!(encode("aü€", Encoding::Windows1252), b"a\xFC\x80");
        // Outside the code page: a question mark, as Excel writes.
        assert_eq!(encode("a\u{4E2D}b", Encoding::Windows1252), b"a?b");
        assert_eq!(
            encode("a\r\n", Encoding::Utf16LeBom),
            b"\xFF\xFEa\x00\r\x00\n\x00"
        );
    }

    #[test]
    fn formatted_text_pads_to_the_column_width() {
        let mut s = sheet_of(&[&["ab"]]);
        s.set_cell(0, 1, Cell::number(42.0));
        let prn = sheet_prn(&s, &Styles::default(), false);
        // Default width 8.43 → 8 characters; the number is right-aligned.
        assert_eq!(prn, "ab            42\r\n");
    }

    #[test]
    fn formatted_text_writes_a_number_whole_past_its_column() {
        let mut styles = Styles {
            xfs: vec![Xf::default()],
            ..Styles::default()
        };
        let mut xf = Xf::default();
        xf.set_code(Some("m/d/yyyy".into()));
        let dated = styles.intern(xf);
        let mut s = Sheet::default();
        s.set_cell(
            0,
            0,
            Cell {
                style: dated,
                ..Cell::number(serial(2024, 1, 15))
            },
        );
        s.set_cell(1, 0, Cell::number(123_456_789.0));
        s.set_cell(2, 0, Cell::text("a long piece of text"));
        let prn = sheet_prn(&s, &styles, false);
        assert_eq!(prn, "1/15/2024\r\n123456789\r\na long p\r\n");
    }

    #[test]
    fn advanced_separators_keep_the_grouping_rule() {
        let opts = TextParse {
            decimal: ',',
            thousands: '.',
            ..TextParse::default()
        };
        let v = |f: &str| {
            convert_field(
                f,
                ColFormat::General,
                &opts,
                &AutoConvert::default(),
                &ctx(),
            )
            .unwrap()
            .entry
            .cell
            .value
        };
        assert_eq!(v("03.04.2024"), CellValue::Text("03.04.2024".into()));
        assert_eq!(v("1.2"), CellValue::Text("1.2".into()));
        assert_eq!(v("1.234,5"), CellValue::Number(1234.5));
        assert_eq!(v("1234,5"), CellValue::Number(1234.5));
        assert_eq!(v("12,5%"), CellValue::Number(0.125));
        // A field that is not number-shaped reads as typed: a date stays one.
        assert_eq!(v("1/2"), CellValue::Number(serial(2024, 1, 2)));
        assert_eq!(v("apple"), CellValue::Text("apple".into()));
    }

    #[test]
    fn a_date_column_reads_only_its_order_and_a_trailing_time() {
        let dmy = ColFormat::Date(DateOrder::Dmy);
        let conv = |f: &str| {
            convert_field(
                f,
                dmy,
                &TextParse::default(),
                &AutoConvert::default(),
                &ctx(),
            )
            .unwrap()
            .entry
        };
        let e = conv("03/04/2024 10:30");
        assert_eq!(
            e.cell.value,
            CellValue::Number(serial(2024, 4, 3) + 10.5 / 24.0)
        );
        assert_eq!(e.format, Some("m/d/yyyy h:mm"));
        // Not a DMY date: text, never re-read as MDY.
        assert_eq!(
            conv("04/13/2024").cell.value,
            CellValue::Text("04/13/2024".into())
        );
        assert_eq!(conv("apple").cell.value, CellValue::Text("apple".into()));
        assert_eq!(conv("03/04/2024").format, Some("m/d/yyyy"));
    }

    #[test]
    fn a_web_page_links_its_files_folder() {
        let mut s = sheet_of(&[&["<a>", "b"]]);
        s.name = "Data".into();
        let page = web_page(&s, &Styles::default(), false, "out.htm");
        assert!(page.htm.contains("href=\"out_files/filelist.xml\""));
        assert!(page.htm.contains("href=\"out_files/stylesheet.css\""));
        assert!(page.htm.contains("<td>&lt;a&gt;</td>"));
        let names: Vec<&str> = page.files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["filelist.xml", "stylesheet.css"]);
        assert!(page.files[0].1.contains("HRef=\"../out.htm\""));
    }
}
