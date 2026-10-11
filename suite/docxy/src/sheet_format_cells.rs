//! Excel's Format Cells over the sheet's selection (#1165): Home › Cells ›
//! Format, Ctrl+1 (⌘1) and the Home tab's Font, Alignment and Number
//! launchers open it.
//!
//! It sits on the tab's [`crate::dialog::DialogStack`] (`format-cells`), so
//! the harness's `dialog-set`/`dialog-click` drive it. The Number tab builds a
//! format code from its category's boxes ([`build`]) and opens on the active
//! cell's code taken apart again ([`decompose`]); its Sample shows the active
//! cell through the code as it stands. The other tabs carry what the sheet
//! models: horizontal alignment, bold, italic, font and fill colour, and the
//! box border. OK applies only what changed since the dialog opened, as one
//! undo step that F4 repeats; Cancel applies nothing.

use crate::dialog::catalog;
use crate::dialog::{ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::{COLOR_NAMES, COLOR_SWATCHES, DocTab, FormatFn, Surface};
use gridcore::sheet::{Align, CellValue, Xf};

/// The Number tab's categories, in Excel's order.
pub(crate) const CATEGORIES: [&str; 11] = [
    "General",
    "Number",
    "Currency",
    "Accounting",
    "Date",
    "Time",
    "Percentage",
    "Fraction",
    "Scientific",
    "Text",
    "Custom",
];
const GENERAL: usize = 0;
const NUMBER: usize = 1;
const CURRENCY: usize = 2;
const ACCOUNTING: usize = 3;
const DATE: usize = 4;
const TIME: usize = 5;
const PERCENTAGE: usize = 6;
const FRACTION: usize = 7;
const SCIENTIFIC: usize = 8;
const TEXT: usize = 9;
const CUSTOM: usize = 10;

/// Excel's most decimal places.
const MAX_DECIMALS: u32 = 30;

/// Currency and Accounting's symbols: the label, and what leads the number.
/// `€`, `£` and `¥` go in Excel's `[$sym-locale]` tags, as Excel writes them:
/// the format runtime refuses them bare.
const SYMBOLS: [(&str, &str); 5] = [
    ("None", ""),
    ("$", "$"),
    ("€", "[$€-2]\\ "),
    ("£", "[$£-809]"),
    ("¥", "[$¥-411]"),
];

/// The Date types, shown as they render [`SAMPLE_SERIAL`].
const DATES: [&str; 13] = [
    "m/d/yyyy",
    "dddd, mmmm d, yyyy",
    "m/d/yy",
    "mm/dd/yy",
    "d-mmm",
    "d-mmm-yy",
    "d-mmm-yyyy",
    "mmm-yy",
    "mmmm-yy",
    "mmmm d, yyyy",
    "m/d/yyyy h:mm",
    "m/d/yyyy h:mm AM/PM",
    "yyyy-mm-dd",
];

/// The Time types, shown as they render [`SAMPLE_SERIAL`]. Not `mm:ss.0`:
/// the runtime draws it as `mm:ss`, so it would show as that item twice.
const TIMES: [&str; 6] = [
    "h:mm:ss AM/PM",
    "h:mm",
    "h:mm AM/PM",
    "h:mm:ss",
    "mm:ss",
    "[h]:mm:ss",
];

/// The Fraction types the format runtime renders: Excel's fixed
/// denominators (halves, quarters, …) it does not.
const FRACTIONS: [(&str, &str); 3] = [
    ("Up to one digit (1/4)", "# ?/?"),
    ("Up to two digits (21/25)", "# ??/??"),
    ("Up to three digits (312/943)", "# ???/???"),
];

/// Excel's own sample for the Date and Time lists: 3/14/2012 1:30:55 PM.
const SAMPLE_SERIAL: f64 = 40982.0 + 48655.0 / 86400.0;

/// The negative number the Negative numbers list shows.
const NEGATIVE_SAMPLE: f64 = -1234.10;

/// The Alignment tab's Horizontal list: [`Align`]'s variants.
const ALIGNS: [(&str, Align); 4] = [
    ("General", Align::General),
    ("Left", Align::Left),
    ("Center", Align::Center),
    ("Right", Align::Right),
];

/// The Number tab's controls: one changed means OK may write a new code.
const NUMBER_CONTROLS: [&str; 10] = [
    "category",
    "decimals",
    "thousands",
    "symbol",
    "negative",
    "date-type",
    "time-type",
    "fraction-type",
    "code",
    "codes",
];

/// What the Number tab's boxes hold.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NumberBoxes {
    pub category: usize,
    pub decimals: u32,
    pub thousands: bool,
    pub symbol: usize,
    pub negative: usize,
    pub date: usize,
    pub time: usize,
    pub fraction: usize,
    /// Custom's Type box.
    pub custom: String,
}

impl Default for NumberBoxes {
    /// Excel's boxes for a General cell: two decimals, `$`, `-1234.10`.
    fn default() -> Self {
        Self {
            category: GENERAL,
            decimals: 2,
            thousands: false,
            symbol: 1,
            negative: 0,
            date: 0,
            time: 0,
            fraction: 0,
            custom: "General".into(),
        }
    }
}

/// `0`, `0.00`, `#,##0.00`: the digits with `dec` decimal places.
fn digits(dec: u32, thousands: bool) -> String {
    let mut s = String::from(if thousands { "#,##0" } else { "0" });
    if dec > 0 {
        s.push('.');
        s.extend(std::iter::repeat_n('0', dec as usize));
    }
    s
}

/// Excel's four negative forms of `x`: `-x`, red `x`, `(x)`, red `(x)`.
fn with_negative(x: &str, negative: usize) -> String {
    match negative {
        0 => x.to_string(),
        1 => format!("{x};[Red]{x}"),
        2 => format!("{x}_);({x})"),
        _ => format!("{x}_);[Red]({x})"),
    }
}

/// The form of `with_negative` Excel's built-in codes 37–40 use, without
/// the `_)` that lines positives up with the parentheses.
fn with_negative_bare(x: &str, negative: usize) -> Option<String> {
    match negative {
        2 => Some(format!("{x};({x})")),
        3 => Some(format!("{x};[Red]({x})")),
        _ => None,
    }
}

/// Number's and Currency's positive part: the digits, after Currency's
/// symbol.
fn positive(b: &NumberBoxes) -> String {
    let dec = b.decimals.min(MAX_DECIMALS);
    if b.category == CURRENCY {
        let sym = SYMBOLS[b.symbol.min(SYMBOLS.len() - 1)].1;
        format!("{sym}{}", digits(dec, true))
    } else {
        digits(dec, b.thousands)
    }
}

/// The format code the boxes make; `""` is General (no code).
pub(crate) fn build(b: &NumberBoxes) -> String {
    let dec = b.decimals.min(MAX_DECIMALS);
    let sym = SYMBOLS[b.symbol.min(SYMBOLS.len() - 1)].1;
    match b.category {
        GENERAL => String::new(),
        NUMBER | CURRENCY => with_negative(&positive(b), b.negative),
        ACCOUNTING => {
            let n = digits(dec, true);
            let zero: String = std::iter::repeat_n('?', dec as usize).collect();
            format!("_({sym}* {n}_);_({sym}* ({n});_({sym}* \"-\"{zero}_);_(@_)")
        }
        DATE => DATES[b.date.min(DATES.len() - 1)].into(),
        TIME => TIMES[b.time.min(TIMES.len() - 1)].into(),
        PERCENTAGE => format!("{}%", digits(dec, false)),
        FRACTION => FRACTIONS[b.fraction.min(FRACTIONS.len() - 1)].1.into(),
        SCIENTIFIC => format!("{}E+00", digits(dec, false)),
        TEXT => "@".into(),
        _ => {
            let c = b.custom.trim();
            if c.eq_ignore_ascii_case("general") {
                String::new()
            } else {
                b.custom.clone()
            }
        }
    }
}

/// Every set of boxes the decimal categories can build, in the order
/// [`decompose`] tries them: Number first, so a Currency with no symbol
/// (the same code) reads back as Number.
fn option_grid() -> impl Iterator<Item = NumberBoxes> {
    let base = NumberBoxes::default;
    let number = (0..=MAX_DECIMALS).flat_map(move |d| {
        [false, true].into_iter().flat_map(move |th| {
            (0..4).map(move |neg| NumberBoxes {
                category: NUMBER,
                decimals: d,
                thousands: th,
                negative: neg,
                ..base()
            })
        })
    });
    let currency = (0..=MAX_DECIMALS).flat_map(move |d| {
        (0..SYMBOLS.len()).flat_map(move |s| {
            (0..4).map(move |neg| NumberBoxes {
                category: CURRENCY,
                decimals: d,
                symbol: s,
                negative: neg,
                ..base()
            })
        })
    });
    let accounting = (0..=MAX_DECIMALS).flat_map(move |d| {
        (0..SYMBOLS.len()).map(move |s| NumberBoxes {
            category: ACCOUNTING,
            decimals: d,
            symbol: s,
            ..base()
        })
    });
    let simple = [PERCENTAGE, SCIENTIFIC].into_iter().flat_map(move |cat| {
        (0..=MAX_DECIMALS).map(move |d| NumberBoxes {
            category: cat,
            decimals: d,
            ..base()
        })
    });
    number.chain(currency).chain(accounting).chain(simple)
}

/// The boxes that show `code` (a cell's `Xf::code`; `None` is General).
/// A code no category builds opens as Custom; every box keeps the code in
/// Custom's Type box.
pub(crate) fn decompose(code: Option<&str>) -> NumberBoxes {
    let raw = code.unwrap_or("");
    if raw.trim().is_empty() || raw.trim().eq_ignore_ascii_case("general") {
        return NumberBoxes::default();
    }
    let keep = |b: NumberBoxes| NumberBoxes {
        custom: raw.to_string(),
        ..b
    };
    // `$` also comes quoted or escaped (newer Excel writes `"$"#,##0.00`),
    // and so do parentheses: Excel writes out the built-ins with no number
    // of their own here (5–8, 41–44) as `_("$"* \(#,##0.00\)`.
    let norm = raw
        .replace("\"$\"", "$")
        .replace("\\$", "$")
        .replace("\\(", "(")
        .replace("\\)", ")");
    // The Accounting code the ribbon's Number list writes has no text section.
    let with_text = format!("{norm};_(@_)");
    for b in option_grid() {
        let built = build(&b);
        let bare = match b.category {
            NUMBER | CURRENCY => with_negative_bare(&positive(&b), b.negative),
            _ => None,
        };
        let accounting = b.category == ACCOUNTING && built == with_text;
        if built == norm || bare.as_deref() == Some(norm.as_str()) || accounting {
            return keep(b);
        }
    }
    let pick = |category: usize, at: Option<usize>| {
        at.map(|i| {
            let mut b = NumberBoxes {
                category,
                ..NumberBoxes::default()
            };
            match category {
                DATE => b.date = i,
                TIME => b.time = i,
                _ => b.fraction = i,
            }
            keep(b)
        })
    };
    if let Some(b) = pick(DATE, DATES.iter().position(|c| *c == raw))
        .or_else(|| pick(TIME, TIMES.iter().position(|c| *c == raw)))
        .or_else(|| pick(FRACTION, FRACTIONS.iter().position(|c| c.1 == raw)))
    {
        return b;
    }
    keep(NumberBoxes {
        category: if raw == "@" { TEXT } else { CUSTOM },
        ..NumberBoxes::default()
    })
}

/// `value` rendered through `code` ("" is General), as the grid shows it.
fn render(code: &str, value: &CellValue, date1904: bool) -> String {
    let mut xf = Xf::default();
    xf.set_code((!code.is_empty()).then(|| code.to_string()));
    gridcore::sheet::format_with(&xf, value, date1904)
}

/// A number through `code`, for the lists that show their codes' output.
fn shown(code: &str, n: f64) -> String {
    render(code, &CellValue::Number(n), false)
}

/// The Negative numbers list for the boxes as they stand: `-1,234.10`,
/// `1,234.10 (red)`, `(1,234.10)`, `(1,234.10) (red)`. The grid does not
/// draw a format's `[Red]`, so the red forms say so in words.
fn negative_items(b: &NumberBoxes) -> Vec<String> {
    (0..4)
        .map(|neg| {
            let code = build(&NumberBoxes {
                negative: neg,
                category: if b.category == CURRENCY {
                    CURRENCY
                } else {
                    NUMBER
                },
                ..b.clone()
            });
            let s = shown(&code, NEGATIVE_SAMPLE);
            if neg % 2 == 1 {
                format!("{s} (red)")
            } else {
                s
            }
        })
        .collect()
}

// ---- reading the dialog ----------------------------------------------------

fn choice_of(v: Option<&Value>) -> usize {
    match v {
        Some(Value::Choice(Some(i))) => *i,
        _ => 0,
    }
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::Text(t)) => t.clone(),
        _ => String::new(),
    }
}

/// Decimal places as typed, held to 0..=30 (a part-typed number is 0).
fn decimals_of(v: Option<&Value>) -> u32 {
    text_of(v)
        .trim()
        .parse::<f64>()
        .map_or(0, |n| n.clamp(0.0, f64::from(MAX_DECIMALS)) as u32)
}

/// The Number tab's boxes, through `get` (the values now, or as opened).
fn boxes<'a>(get: impl Fn(&str) -> Option<&'a Value>) -> NumberBoxes {
    NumberBoxes {
        category: choice_of(get("category")),
        decimals: decimals_of(get("decimals")),
        thousands: get("thousands") == Some(&Value::Bool(true)),
        symbol: choice_of(get("symbol")),
        negative: choice_of(get("negative")),
        date: choice_of(get("date-type")),
        time: choice_of(get("time-type")),
        fraction: choice_of(get("fraction-type")),
        custom: text_of(get("code")),
    }
}

fn now(d: &Dialog) -> NumberBoxes {
    boxes(|n| d.value(n))
}

fn opened(d: &Dialog) -> NumberBoxes {
    boxes(|n| {
        d.controls
            .iter()
            .position(|c| c.name == n)
            .and_then(|i| d.opened.get(i))
    })
}

/// The active cell the Sample shows, kept in the hidden `cell` control
/// (a reaction has only the dialog): the date system's digit, then `n` and
/// a number, `t` and a text, or `s` and what any format shows; empty for an
/// empty cell.
fn encode_cell(value: &CellValue, date1904: bool) -> String {
    let sys = if date1904 { '1' } else { '0' };
    match value {
        CellValue::Empty => String::new(),
        CellValue::Number(n) => format!("{sys}n{n}"),
        CellValue::Text(t) => format!("{sys}t{t}"),
        other => format!("{sys}s{}", render("", other, date1904)),
    }
}

/// The Sample: the active cell through the code the boxes make.
fn sample(d: &Dialog, b: &NumberBoxes) -> String {
    let cell = text_of(d.value("cell"));
    let mut chars = cell.chars();
    let date1904 = chars.next() == Some('1');
    let kind = chars.next();
    let rest: String = chars.collect();
    let value = match kind {
        Some('n') => CellValue::Number(rest.parse().unwrap_or(0.0)),
        Some('t') => CellValue::Text(rest),
        Some(_) => return rest,
        None => return String::new(),
    };
    render(&build(b), &value, date1904)
}

/// Show the boxes the category uses, follow a pick from the code list into
/// the Type box, and redraw the Negative numbers list and the Sample.
fn react(d: &mut Dialog, i: usize, _before: &Value) {
    if d.controls.get(i).map(|c| c.name) == Some("codes") {
        let picked = d.controls.get(i).map(Control::text).unwrap_or_default();
        if let Some(c) = d.controls.iter_mut().find(|c| c.name == "code") {
            c.value = Value::Text(picked);
        }
    }
    let b = now(d);
    let cat = b.category;
    let negatives = negative_items(&b);
    let shows = sample(d, &b);
    for c in &mut d.controls {
        match c.name {
            "decimals" => {
                c.visible = matches!(
                    cat,
                    NUMBER | CURRENCY | ACCOUNTING | PERCENTAGE | SCIENTIFIC
                )
            }
            "thousands" => c.visible = cat == NUMBER,
            "symbol" => c.visible = matches!(cat, CURRENCY | ACCOUNTING),
            "negative" => {
                c.visible = matches!(cat, NUMBER | CURRENCY);
                c.items = negatives.clone();
            }
            "date-type" => c.visible = cat == DATE,
            "time-type" => c.visible = cat == TIME,
            "fraction-type" => c.visible = cat == FRACTION,
            "code" | "codes" => c.visible = cat == CUSTOM,
            "sample" => c.value = Value::Text(shows.clone()),
            _ => {}
        }
    }
}

// ---- the dialog ------------------------------------------------------------

/// The Custom list: Excel's built-in codes, then every other code the
/// workbook's styles use, each once.
fn code_list(xfs: &[Xf]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let builtins = (0..=49).filter_map(gridcore::numfmt::builtin_code);
    let used = xfs.iter().filter_map(|x| x.code.as_deref());
    for code in builtins.map(|c: &str| c).chain(used) {
        let code = if code.trim().eq_ignore_ascii_case("general") {
            "General"
        } else {
            code
        };
        if !code.trim().is_empty() && !out.iter().any(|c| c == code) {
            out.push(code.to_string());
        }
    }
    out
}

/// A colour list: `none` (Automatic, No Color), the swatches by name, and
/// the cell's own colour when it is none of them, which OK leaves alone.
/// Returns the items and the cell's place in them.
fn colour_list(none: &str, rgb: Option<(u8, u8, u8)>, unresolved: bool) -> (Vec<String>, usize) {
    let mut items = vec![none.to_string()];
    items.extend(COLOR_NAMES.iter().map(|n| n.to_string()));
    let at = match rgb {
        None if !unresolved => 0,
        None => {
            items.push("Theme colour".into());
            items.len() - 1
        }
        Some((r, g, b)) => {
            let hex = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
            match COLOR_SWATCHES.iter().position(|&c| c == hex) {
                Some(k) => k + 1,
                None => {
                    items.push(format!("Custom (#{hex:06X})"));
                    items.len() - 1
                }
            }
        }
    };
    (items, at)
}

/// The colour a list's pick sets: `None` for its first item, a swatch, or
/// `Err` for the cell's own colour (left as it was).
fn colour_of(pick: usize) -> Result<Option<(u8, u8, u8)>, ()> {
    match pick {
        0 => Ok(None),
        k => COLOR_SWATCHES
            .get(k - 1)
            .map(|&c| Some(((c >> 16) as u8, (c >> 8) as u8, c as u8)))
            .ok_or(()),
    }
}

/// Format Cells over the selection, showing its active cell. Refused in
/// Protected View or a final workbook and on a protected sheet.
pub(crate) fn dialog(tab: &DocTab) -> Result<Dialog, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Format Cells needs a spreadsheet".into());
    };
    if tab.access.locked() {
        return Err(tab.access.locked_status().into());
    }
    if v.sheet().is_protected() {
        return Err(crate::sheet_goto::SHEET_PROTECTED.into());
    }
    let (r, c) = v.sel;
    let cell = v.sheet().cell(r, c);
    let xf = v.pkg.workbook.styles.xf(cell.map_or(0, |cl| cl.style));
    let value = cell.map(|cl| cl.value.clone()).unwrap_or_default();
    let b = decompose(xf.code.as_deref());
    let mut d = Dialog::message(
        catalog::FORMAT_CELLS,
        "Format Cells",
        String::new(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::FormatCells { sheet: v.active },
    );
    d.text = None;
    d.tabs = ["Number", "Alignment", "Font", "Fill", "Border"]
        .map(String::from)
        .to_vec();
    let page = |mut c: Control, p: usize| {
        c.page = Some(p);
        c
    };
    let dropdown = |name, label: &str, items: Vec<String>, at: usize| {
        let mut c = Control::new(name, label, ControlKind::Dropdown, Value::Choice(Some(at)));
        c.items = items;
        c
    };
    let check = |name, label: &str, on: bool| {
        Control::new(name, label, ControlKind::Checkbox, Value::Bool(on))
    };
    let strings = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut cell_ctl = Control::new(
        "cell",
        "",
        ControlKind::Label,
        Value::Text(encode_cell(&value, v.pkg.workbook.date1904)),
    );
    cell_ctl.visible = false;
    let codes = code_list(&v.pkg.workbook.styles.xfs);
    let code_at = codes.iter().position(|c| *c == b.custom);
    let mut codes_ctl = dropdown("codes", "Codes:", codes, 0);
    codes_ctl.value = Value::Choice(code_at);
    let align_at = ALIGNS.iter().position(|a| a.1 == xf.align).unwrap_or(0);
    let (font_items, font_at) = colour_list("Automatic", xf.color, xf.color_unresolved);
    let (fill_items, fill_at) = colour_list("No Color", xf.fill, xf.fill_unresolved);
    d.controls = vec![
        page(
            dropdown("category", "Category:", strings(&CATEGORIES), b.category),
            0,
        ),
        page(
            Control::new(
                "sample",
                "Sample",
                ControlKind::Label,
                Value::Text(String::new()),
            ),
            0,
        ),
        page(
            Control::new(
                "decimals",
                "Decimal places:",
                ControlKind::Number,
                Value::Text(b.decimals.to_string()),
            ),
            0,
        ),
        page(check("thousands", "Use 1000 Separator (,)", b.thousands), 0),
        page(
            dropdown(
                "symbol",
                "Symbol:",
                SYMBOLS.iter().map(|s| s.0.to_string()).collect(),
                b.symbol,
            ),
            0,
        ),
        page(
            dropdown(
                "negative",
                "Negative numbers:",
                negative_items(&b),
                b.negative,
            ),
            0,
        ),
        page(
            dropdown(
                "date-type",
                "Type:",
                DATES.iter().map(|c| shown(c, SAMPLE_SERIAL)).collect(),
                b.date,
            ),
            0,
        ),
        page(
            dropdown(
                "time-type",
                "Type:",
                TIMES.iter().map(|c| shown(c, SAMPLE_SERIAL)).collect(),
                b.time,
            ),
            0,
        ),
        page(
            dropdown(
                "fraction-type",
                "Type:",
                FRACTIONS.iter().map(|f| f.0.to_string()).collect(),
                b.fraction,
            ),
            0,
        ),
        page(
            Control::new(
                "code",
                "Type:",
                ControlKind::Text,
                Value::Text(b.custom.clone()),
            ),
            0,
        ),
        page(codes_ctl, 0),
        page(
            dropdown(
                "horizontal",
                "Horizontal:",
                ALIGNS.iter().map(|a| a.0.to_string()).collect(),
                align_at,
            ),
            1,
        ),
        page(check("bold", "Bold", xf.bold), 2),
        page(check("italic", "Italic", xf.italic), 2),
        page(dropdown("color", "Color:", font_items, font_at), 2),
        page(
            dropdown("fill", "Background Color:", fill_items, fill_at),
            3,
        ),
        page(check("outline", "Outline", xf.border), 4),
        cell_ctl,
    ];
    d.react = Some(crate::dialog::Reaction(react));
    react(&mut d, usize::MAX, &Value::Bool(false));
    d.mark_opened();
    Ok(d)
}

/// Open Format Cells on the tab, unless it is already the top dialog
/// (Ctrl+1 or a launcher again leaves the one that is up).
pub(crate) fn open(tab: &mut DocTab) -> Result<(), String> {
    if tab
        .dialogs
        .top()
        .is_some_and(|d| d.id == catalog::FORMAT_CELLS)
    {
        return Ok(());
    }
    let d = dialog(tab)?;
    tab.dialogs.push(d);
    Ok(())
}

/// What OK writes: one setter of every property whose controls changed
/// since the dialog opened, `None` when nothing did, or why the code typed
/// can't be used.
fn setter(d: &Dialog) -> Result<Option<FormatFn>, String> {
    let mut parts: Vec<FormatFn> = Vec::new();
    if NUMBER_CONTROLS.iter().any(|n| d.changed(n)) {
        let (now, was) = (now(d), opened(d));
        let code = build(&now);
        // A Custom code counts as typed, against the cell's own code; the
        // boxes' codes compare as built, so a box set back writes nothing.
        let differs = if now.category == CUSTOM {
            now.custom != was.custom
        } else {
            code != build(&was)
        };
        if differs {
            if !code.is_empty() && gridcore::numfmt::parse_format(&code).is_none() {
                return Err(format!("docxy cannot use this number format: {code}"));
            }
            parts.push(crate::numfmt_setter(&code));
        }
    }
    let choice = |n: &str| choice_of(d.value(n));
    let on = |n: &str| d.value(n) == Some(&Value::Bool(true));
    if d.changed("horizontal") {
        let a = ALIGNS[choice("horizontal").min(ALIGNS.len() - 1)].1;
        parts.push(std::rc::Rc::new(move |x: &mut Xf| x.align = a));
    }
    if d.changed("bold") {
        let b = on("bold");
        parts.push(std::rc::Rc::new(move |x: &mut Xf| x.bold = b));
    }
    if d.changed("italic") {
        let i = on("italic");
        parts.push(std::rc::Rc::new(move |x: &mut Xf| x.italic = i));
    }
    if d.changed("color") {
        if let Ok(rgb) = colour_of(choice("color")) {
            parts.push(std::rc::Rc::new(move |x: &mut Xf| {
                x.color = rgb;
                x.color_unresolved = false;
            }));
        }
    }
    if d.changed("fill") {
        if let Ok(rgb) = colour_of(choice("fill")) {
            parts.push(std::rc::Rc::new(move |x: &mut Xf| {
                x.fill = rgb;
                x.fill_unresolved = false;
            }));
        }
    }
    if d.changed("outline") {
        let b = on("outline");
        parts.push(std::rc::Rc::new(move |x: &mut Xf| x.border = b));
    }
    Ok(match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(std::rc::Rc::new(move |x: &mut Xf| {
            for p in &parts {
                p(x);
            }
        })),
    })
}

/// OK on Format Cells: what changed, to every cell of the selection, as one
/// undo step that F4 repeats. A code the format runtime can't use keeps the
/// dialog open. `None` for any other press or dialog (Cancel closes through
/// the stack).
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    let DialogOwner::FormatCells { sheet } = top.owner else {
        return None;
    };
    if !button.replace('&', "").trim().eq_ignore_ascii_case("OK") {
        return None;
    }
    let f = match setter(top) {
        Ok(f) => f,
        Err(e) => return Some(Err(e)),
    };
    if tab.access.locked() {
        return Some(Err(tab.access.locked_status().into()));
    }
    let Surface::Sheet(v) = &mut tab.surface else {
        return Some(Err("Format Cells needs a spreadsheet".into()));
    };
    if v.pkg
        .workbook
        .sheets
        .get(sheet)
        .is_none_or(|s| s.is_protected())
    {
        return Some(Err(crate::sheet_goto::SHEET_PROTECTED.into()));
    }
    tab.dialogs.pop();
    let Some(f) = f else {
        return Some(Ok(()));
    };
    v.push_undo();
    v.apply_format(f);
    v.prune_circles();
    tab.set_dirty();
    Some(Ok(()))
}

#[cfg(test)]
mod tests;
