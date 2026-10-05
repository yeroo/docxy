//! AutoFill's series rules, as Excel has them (#668): what a dragged fill
//! handle, Home › Fill › Series and the Auto Fill Options write.
//!
//! A source line (one column of a fill down, one row of a fill right) is
//! read seed by seed. Each seed has a pattern: a number, a date or time, text
//! with a number in it (`Item 1`, `A001`, `1 apple`), an ordinal (`1st`), a
//! quarter (`Q3`, `Qtr 4`), an item of a built-in or custom list (`Wed`,
//! `November`), or none (copied as it is). Each seed position runs its own
//! series over the seeds of the line that share its pattern, so `Item 1, x`
//! fills `Item 2, x, Item 3, x`.

use crate::sheet::{Cell, CellValue, NumFmt, Styles, classify_format_code, parts_to_serial};

use super::Rect;

/// How a fill writes its cells: what the Auto Fill Options button, the
/// fill handle's right-drag menu and the Series dialog offer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FillKind {
    /// What a plain drag does: a series where the source reads as one, a
    /// copy otherwise (one plain number is copied).
    #[default]
    Auto,
    /// Copy Cells.
    Copy,
    /// Fill Series: as `Auto`, but one plain number counts up by 1.
    Series,
    /// Fill Formatting Only: the source's styles, the destination's values.
    FormatsOnly,
    /// Fill Without Formatting: the series' values, the destination's styles.
    WithoutFormatting,
    /// Fill Days, Weekdays, Months, Years: date seeds step by that unit.
    Days,
    Weekdays,
    Months,
    Years,
    /// Linear Trend: numbers along their least-squares line.
    LinearTrend,
    /// Growth Trend: numbers along their least-squares exponential.
    GrowthTrend,
}

impl FillKind {
    /// Every kind, in the order Excel's Auto Fill Options lists them.
    pub const ALL: [FillKind; 11] = [
        FillKind::Copy,
        FillKind::Series,
        FillKind::FormatsOnly,
        FillKind::WithoutFormatting,
        FillKind::Days,
        FillKind::Weekdays,
        FillKind::Months,
        FillKind::Years,
        FillKind::LinearTrend,
        FillKind::GrowthTrend,
        FillKind::Auto,
    ];

    /// The menu label (Excel's).
    pub fn label(self) -> &'static str {
        match self {
            FillKind::Auto => "Auto Fill",
            FillKind::Copy => "Copy Cells",
            FillKind::Series => "Fill Series",
            FillKind::FormatsOnly => "Fill Formatting Only",
            FillKind::WithoutFormatting => "Fill Without Formatting",
            FillKind::Days => "Fill Days",
            FillKind::Weekdays => "Fill Weekdays",
            FillKind::Months => "Fill Months",
            FillKind::Years => "Fill Years",
            FillKind::LinearTrend => "Linear Trend",
            FillKind::GrowthTrend => "Growth Trend",
        }
    }

    /// The kind a label names, any case; also takes the short ids `copy`,
    /// `series`, `formats`, `values`, `days`, `weekdays`, `months`, `years`,
    /// `linear`, `growth` and `auto`.
    pub fn from_label(s: &str) -> Option<FillKind> {
        let s = s.trim();
        let short = [
            ("auto", FillKind::Auto),
            ("copy", FillKind::Copy),
            ("series", FillKind::Series),
            ("formats", FillKind::FormatsOnly),
            ("values", FillKind::WithoutFormatting),
            ("days", FillKind::Days),
            ("weekdays", FillKind::Weekdays),
            ("months", FillKind::Months),
            ("years", FillKind::Years),
            ("linear", FillKind::LinearTrend),
            ("growth", FillKind::GrowthTrend),
        ];
        short
            .iter()
            .find(|(id, _)| id.eq_ignore_ascii_case(s))
            .map(|&(_, k)| k)
            .or_else(|| {
                FillKind::ALL
                    .into_iter()
                    .find(|k| k.label().eq_ignore_ascii_case(s))
            })
    }

    /// Whether the kind only applies to date seeds.
    pub fn is_date_unit(self) -> bool {
        matches!(
            self,
            FillKind::Days | FillKind::Weekdays | FillKind::Months | FillKind::Years
        )
    }
}

use crate::numfmt::{DAYS, MONTHS};

/// Excel's built-in custom lists, as File › Options › Edit Custom Lists shows
/// them: short and long day names, short and long month names.
pub fn builtin_lists() -> Vec<Vec<String>> {
    let short = |names: &[&str]| names.iter().map(|n| n[..3].to_string()).collect();
    let long = |names: &[&str]| names.iter().map(|n| n.to_string()).collect();
    vec![short(&DAYS), long(&DAYS), short(&MONTHS), long(&MONTHS)]
}

/// How a list item was spelled, kept by its continuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Lower,
    Title,
    Upper,
    /// As the list spells it.
    AsListed,
}

fn case_of(s: &str) -> Case {
    let letters: Vec<char> = s.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return Case::AsListed;
    }
    if letters.len() > 1 && letters.iter().all(|c| c.is_uppercase()) {
        Case::Upper
    } else if letters.iter().all(|c| c.is_lowercase()) {
        Case::Lower
    } else if letters[0].is_uppercase() && letters[1..].iter().all(|c| c.is_lowercase()) {
        Case::Title
    } else {
        Case::AsListed
    }
}

fn apply_case(s: &str, case: Case) -> String {
    match case {
        Case::Lower => s.to_lowercase(),
        Case::Upper => s.to_uppercase(),
        Case::Title => {
            let mut out = String::new();
            for (i, ch) in s.chars().enumerate() {
                if i == 0 {
                    out.extend(ch.to_uppercase());
                } else {
                    out.extend(ch.to_lowercase());
                }
            }
            out
        }
        Case::AsListed => s.to_string(),
    }
}

/// Whether a date-formatted number is a date (or date-time), or a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Temporal {
    Date,
    Time,
}

/// What a seed reads as.
#[derive(Clone, Debug, PartialEq)]
enum Seed {
    Num(f64),
    Date(f64, Temporal),
    /// Text counting by the number in it: `prefix`, the digits (zero-padded
    /// to `width`), `suffix`.
    Text {
        prefix: String,
        n: i64,
        width: usize,
        suffix: String,
    },
    /// `1st`, `2ND`: the suffix's case is kept.
    Ordinal {
        n: i64,
        upper: bool,
    },
    /// `Q3`, `Qtr 4`, `Quarter 1`: `prefix` is everything before the digit.
    Quarter {
        prefix: String,
        n: i64,
    },
    /// Item `idx` of list `list`.
    List {
        list: usize,
        idx: usize,
        case: Case,
    },
    /// Copied as it is.
    Other,
}

/// The pattern seeds group by: seeds with the same key form one series.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Num,
    Date(Temporal),
    Text(String, String),
    Ordinal,
    Quarter(String),
    List(usize),
}

impl Seed {
    fn key(&self) -> Option<Key> {
        Some(match self {
            Seed::Num(_) => Key::Num,
            Seed::Date(_, t) => Key::Date(*t),
            Seed::Text { prefix, suffix, .. } => Key::Text(prefix.clone(), suffix.clone()),
            Seed::Ordinal { .. } => Key::Ordinal,
            Seed::Quarter { prefix, .. } => Key::Quarter(prefix.to_lowercase()),
            Seed::List { list, .. } => Key::List(*list),
            Seed::Other => return None,
        })
    }

    /// The seed's place on its series' number line.
    fn value(&self) -> f64 {
        match self {
            Seed::Num(n) | Seed::Date(n, _) => *n,
            Seed::Text { n, .. } | Seed::Ordinal { n, .. } => *n as f64,
            Seed::Quarter { n, .. } => (*n - 1) as f64,
            Seed::List { idx, .. } => *idx as f64,
            Seed::Other => 0.0,
        }
    }
}

/// The context seeds are read in.
pub(crate) struct SeedCtx<'a> {
    pub styles: &'a Styles,
    pub date1904: bool,
    /// The user's custom lists; the built-in ones come first.
    pub lists: &'a [Vec<String>],
}

impl SeedCtx<'_> {
    fn all_lists(&self) -> Vec<Vec<String>> {
        let mut all = builtin_lists();
        all.extend(self.lists.iter().filter(|l| !l.is_empty()).cloned());
        all
    }
}

fn temporal(styles: &Styles, style: u32) -> Option<Temporal> {
    let xf = styles.xf(style);
    let class = match xf.code.as_deref() {
        Some(c) => classify_format_code(c),
        None => xf.numfmt,
    };
    match class {
        NumFmt::Date | NumFmt::DateTime => Some(Temporal::Date),
        NumFmt::Time => Some(Temporal::Time),
        _ => None,
    }
}

fn read_seed(cell: Option<&Cell>, ctx: &SeedCtx, lists: &[Vec<String>]) -> Seed {
    let Some(cell) = cell else {
        return Seed::Other;
    };
    if cell.formula.is_some() || cell.f_attrs.is_some() {
        return Seed::Other;
    }
    match &cell.value {
        CellValue::Number(n) => match temporal(ctx.styles, cell.style) {
            Some(t) => Seed::Date(*n, t),
            None => Seed::Num(*n),
        },
        CellValue::Text(s) => read_text_seed(s, lists),
        _ => Seed::Other,
    }
}

fn read_text_seed(s: &str, lists: &[Vec<String>]) -> Seed {
    // A list item first: `Mon`, `November`, a custom list's entry.
    for (li, list) in lists.iter().enumerate() {
        if let Some(idx) = list.iter().position(|item| item.eq_ignore_ascii_case(s)) {
            let case = if list[idx] == s {
                Case::AsListed
            } else {
                case_of(s)
            };
            return Seed::List {
                list: li,
                idx,
                case,
            };
        }
    }
    // An ordinal: digits then st/nd/rd/th.
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if digits_end > 0 && digits_end < s.len() {
        let suffix = &s[digits_end..];
        if ["st", "nd", "rd", "th"]
            .iter()
            .any(|x| x.eq_ignore_ascii_case(suffix))
        {
            if let Ok(n) = s[..digits_end].parse::<i64>() {
                return Seed::Ordinal {
                    n,
                    upper: suffix.chars().all(|c| c.is_uppercase()),
                };
            }
        }
    }
    // A quarter: Q, Qtr or Quarter (any case, an optional space), then 1-4.
    if let Some(last) = s.chars().last() {
        if ('1'..='4').contains(&last) && s.len() >= 2 {
            let prefix = &s[..s.len() - 1];
            let word = prefix.trim_end();
            if ["q", "qtr", "quarter"]
                .iter()
                .any(|w| w.eq_ignore_ascii_case(word))
                && (prefix.len() - word.len()) <= 1
            {
                return Seed::Quarter {
                    prefix: prefix.to_string(),
                    n: i64::from(last as u8 - b'0'),
                };
            }
        }
    }
    // A trailing number, else a leading one.
    let bytes = s.as_bytes();
    let tail_start = bytes
        .iter()
        .rposition(|b| !b.is_ascii_digit())
        .map_or(0, |i| i + 1);
    if tail_start < s.len() && tail_start > 0 {
        let digits = &s[tail_start..];
        if let Ok(n) = digits.parse::<i64>() {
            return Seed::Text {
                prefix: s[..tail_start].to_string(),
                n,
                width: digits.len(),
                suffix: String::new(),
            };
        }
    }
    if digits_end > 0 && digits_end < s.len() {
        if let Ok(n) = s[..digits_end].parse::<i64>() {
            return Seed::Text {
                prefix: String::new(),
                n,
                width: digits_end,
                suffix: s[digits_end..].to_string(),
            };
        }
    }
    Seed::Other
}

fn ordinal_suffix(n: i64) -> &'static str {
    let n = n.abs();
    if (11..=13).contains(&(n % 100)) {
        return "th";
    }
    match n % 10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    }
}

/// Write a series value `v` in its group's pattern, styled like `seed_cell`.
fn render(seed: &Seed, v: f64, seed_cell: Option<&Cell>, lists: &[Vec<String>]) -> Cell {
    let style = seed_cell.map_or(0, |c| c.style);
    let mut cell = match seed {
        Seed::Num(_) | Seed::Date(..) => Cell::number(v),
        Seed::Text {
            prefix,
            width,
            suffix,
            ..
        } => {
            let n = (v.round() as i64).abs();
            Cell::text(&format!("{prefix}{n:0width$}{suffix}"))
        }
        Seed::Ordinal { upper, .. } => {
            let n = (v.round() as i64).abs();
            let sfx = ordinal_suffix(n);
            let sfx = if *upper {
                sfx.to_uppercase()
            } else {
                sfx.to_string()
            };
            Cell::text(&format!("{n}{sfx}"))
        }
        Seed::Quarter { prefix, .. } => {
            let q = (v.round() as i64).rem_euclid(4) + 1;
            Cell::text(&format!("{prefix}{q}"))
        }
        Seed::List { list, case, .. } => {
            let items = &lists[*list];
            let idx = (v.round() as i64).rem_euclid(items.len() as i64) as usize;
            Cell::text(&apply_case(&items[idx], *case))
        }
        Seed::Other => seed_cell.cloned().unwrap_or_default(),
    };
    cell.style = style;
    cell
}

/// One output of [`extend_line`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LineOut {
    /// A copy of source cell `k` (the caller re-bases its formula).
    Copy(usize),
    /// A new value, styled like its seed.
    Value(Cell),
}

/// The least-squares line through `(i, ys[i])`: (intercept, slope).
fn linear_fit(ys: &[f64]) -> (f64, f64) {
    let n = ys.len() as f64;
    let mx = (n - 1.0) / 2.0;
    let my = ys.iter().sum::<f64>() / n;
    let mut sxy = 0.0;
    let mut sxx = 0.0;
    for (i, y) in ys.iter().enumerate() {
        let dx = i as f64 - mx;
        sxy += dx * (y - my);
        sxx += dx * dx;
    }
    let slope = if sxx == 0.0 { 0.0 } else { sxy / sxx };
    (my - slope * mx, slope)
}

/// Whether every step of `ys` equals the first, within a relative 1e-9
/// (absolute near zero): such a run extends by its step exactly.
fn exact_step(ys: &[f64]) -> Option<f64> {
    let step = ys.get(1)? - ys[0];
    let tol = 1e-9 * step.abs().max(1e-300);
    ys.windows(2)
        .all(|w| ((w[1] - w[0]) - step).abs() <= tol.max(1e-12))
        .then_some(step)
}

/// The linear series through `ys` at index `n`: an exact run by its step
/// from the last seed, anything else along the least-squares line. One seed
/// steps by `lone`.
fn linear_at(ys: &[f64], n: f64, lone: f64) -> f64 {
    let m = ys.len();
    if m == 1 {
        return ys[0] + lone * n;
    }
    if let Some(step) = exact_step(ys) {
        return ys[m - 1] + step * (n - (m - 1) as f64);
    }
    let (a, b) = linear_fit(ys);
    a + b * n
}

/// The growth series through `ys` at index `n`: two seeds by their ratio,
/// three or more along the least-squares exponential. A non-positive seed
/// has no exponential: those fall back to the linear series.
fn growth_at(ys: &[f64], n: f64) -> f64 {
    let m = ys.len();
    if m == 1 {
        return ys[0];
    }
    if ys.iter().all(|&y| y > 0.0) {
        if m == 2 {
            return ys[0] * (ys[1] / ys[0]).powf(n);
        }
        let logs: Vec<f64> = ys.iter().map(|y| y.ln()).collect();
        let (a, b) = linear_fit(&logs);
        return (a + b * n).exp();
    }
    linear_at(ys, n, 1.0)
}

/// Calendar parts of a date serial: (year, month, day, seconds into the day).
fn ymd(serial: f64, date1904: bool) -> Option<(i64, u32, u32, u32)> {
    let p = crate::sheet::serial_to_parts(serial, date1904)?;
    Some((
        p.year,
        p.month,
        p.day,
        p.hour * 3600 + p.minute * 60 + p.second,
    ))
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `serial` moved by `months`, its day clamped to the target month's end.
pub(crate) fn add_months(serial: f64, months: i64, date1904: bool) -> f64 {
    let Some((y, m, d, secs)) = ymd(serial, date1904) else {
        return serial;
    };
    let total = y * 12 + i64::from(m - 1) + months;
    let (ny, nm) = (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32);
    let nd = d.min(days_in_month(ny, nm));
    parts_to_serial(ny, nm, nd, secs, date1904)
}

/// The last serial a date can have (9999-12-31).
const MAX_SERIAL: f64 = 2_958_465.0;

/// Whether `serial` is a date Excel can show; the weekday arithmetic
/// leaves anything else alone (#707 r3 m2).
fn in_date_range(serial: f64) -> bool {
    serial.is_finite() && (0.0..MAX_SERIAL + 1.0).contains(&serial)
}

/// Whether whole day `day` is a Saturday or Sunday, in the 1900 date
/// system (serial 1, 1900-01-01, a Sunday in Excel's reckoning) or the
/// 1904 one (serial 0, 1904-01-01, a Friday).
fn weekend_day(day: i64, date1904: bool) -> bool {
    let shift = if date1904 { 5 } else { -1 };
    let dow = (day + shift).rem_euclid(7);
    dow == 0 || dow == 6
}

/// Whether the serial falls on a Saturday or Sunday (the tests' walk).
#[cfg(test)]
fn weekend(serial: f64, date1904: bool) -> bool {
    weekend_day(serial.floor() as i64, date1904)
}

/// `serial` moved by `k` weekdays (Monday to Friday), in constant time and
/// whole days (#707 r2 M2, r3 m2): a start on a weekend is first taken back
/// to the weekday before it in the walk's direction (which moves to the
/// same weekdays), every whole five weekdays is a week, and only the rest
/// is walked. A serial that is no date, or a move off the calendar, is not
/// walked.
pub(crate) fn add_weekdays(serial: f64, k: i64, date1904: bool) -> f64 {
    if k == 0 || !in_date_range(serial) {
        return serial;
    }
    let frac = serial - serial.floor();
    let dir = k.signum();
    let mut day = serial.floor() as i64;
    while weekend_day(day, date1904) {
        day -= dir;
    }
    let n = k.unsigned_abs();
    let weeks = i128::from(dir) * 7 * i128::from(n / 5);
    let moved = i128::from(day) + weeks;
    if !(0..=MAX_SERIAL as i128 + 7).contains(&moved) {
        return moved as f64 + frac;
    }
    day = moved as i64;
    let mut left = n % 5;
    while left > 0 {
        day += dir;
        if !weekend_day(day, date1904) {
            left -= 1;
        }
    }
    day as f64 + frac
}

/// The weekdays in the whole days `(lo, hi]`, in constant time.
fn weekdays_in(lo: i64, hi: i64, date1904: bool) -> i64 {
    let days = hi - lo;
    let mut n = days / 7 * 5;
    let mut day = lo + days / 7 * 7;
    while day < hi {
        day += 1;
        if !weekend_day(day, date1904) {
            n += 1;
        }
    }
    n
}

/// The weekdays from `a` to `b` as a walk from `a` counts them: those in
/// `(a, b]` going forward, minus those in `[b, a)` going back (#707 r3 m1).
/// 0 for a serial that is no date.
fn weekdays_between(a: f64, b: f64, date1904: bool) -> i64 {
    if !in_date_range(a) || !in_date_range(b) {
        return 0;
    }
    let (da, db) = (a.floor() as i64, b.floor() as i64);
    if db >= da {
        weekdays_in(da, db, date1904)
    } else {
        -weekdays_in(db - 1, da - 1, date1904)
    }
}

/// The whole-month step between date seeds that share a day of month and
/// time of day, when they all differ by it.
fn month_step(ys: &[f64], date1904: bool) -> Option<i64> {
    let parts: Vec<(i64, u32, u32, u32)> = ys
        .iter()
        .map(|&y| ymd(y, date1904))
        .collect::<Option<_>>()?;
    let (_, _, d0, s0) = parts[0];
    if parts.iter().any(|&(_, _, d, s)| d != d0 || s != s0) {
        return None;
    }
    let idx: Vec<i64> = parts
        .iter()
        .map(|&(y, m, _, _)| y * 12 + i64::from(m))
        .collect();
    let step = idx[1] - idx[0];
    (step != 0 && idx.windows(2).all(|w| w[1] - w[0] == step)).then_some(step)
}

/// A date group's value at index `n` for `kind`.
fn date_at(ys: &[f64], n: f64, t: Temporal, kind: FillKind, date1904: bool, dir: f64) -> f64 {
    let m = ys.len();
    let last = ys[m - 1];
    let ahead = n - (m - 1) as f64;
    match kind {
        FillKind::Days => {
            let step = if m == 1 {
                dir
            } else {
                (last - ys[m - 2]).round()
            };
            last + step * ahead
        }
        FillKind::Weekdays => {
            let step = if m == 1 {
                dir as i64
            } else {
                // The weekdays between the last two seeds; two on one
                // weekend still step in their own direction.
                let k = weekdays_between(ys[m - 2], last, date1904);
                match k {
                    0 if last < ys[m - 2] => -1,
                    0 => 1,
                    k => k,
                }
            };
            add_weekdays(last, step * ahead.round() as i64, date1904)
        }
        FillKind::Months | FillKind::Years => {
            let unit = if kind == FillKind::Years { 12 } else { 1 };
            let step = if m == 1 {
                unit * dir as i64
            } else {
                let a = ymd(ys[m - 2], date1904);
                let b = ymd(last, date1904);
                match (a, b) {
                    (Some((ya, ma, ..)), Some((yb, mb, ..))) => {
                        let d = (yb * 12 + i64::from(mb)) - (ya * 12 + i64::from(ma));
                        let d = d / unit * unit;
                        if d == 0 { unit } else { d }
                    }
                    _ => unit,
                }
            };
            // From the first seed, so a clamped month end does not stick:
            // 31 Jan, 29 Feb, 31 Mar.
            add_months(ys[0], step * n.round() as i64, date1904)
        }
        FillKind::LinearTrend => linear_at(ys, n, dir),
        FillKind::GrowthTrend => growth_at(ys, n),
        _ => {
            if m == 1 {
                let lone = if t == Temporal::Time { dir / 24.0 } else { dir };
                return ys[0] + lone * n;
            }
            if let Some(step) = month_step(ys, date1904) {
                return add_months(ys[0], step * n.round() as i64, date1904);
            }
            linear_at(ys, n, 1.0)
        }
    }
}

/// Produce `count` cells continuing a source line `src` (seed 0 nearest the
/// start of the fill; a fill up or left passes its line reversed). A
/// formula, a blank or text that does not count is copied (`LineOut::Copy`,
/// cycling through the source); every other seed continues its series.
pub(crate) fn extend_line(
    src: &[Option<Cell>],
    count: usize,
    kind: FillKind,
    ctrl: bool,
    backwards: bool,
    ctx: &SeedCtx,
) -> Vec<LineOut> {
    let len = src.len();
    if len == 0 {
        return Vec::new();
    }
    let copy_all = || (0..count).map(|k| LineOut::Copy(k % len)).collect();
    let lists = ctx.all_lists();
    let seeds: Vec<Seed> = src
        .iter()
        .map(|c| read_seed(c.as_ref(), ctx, &lists))
        .collect();
    // A lone seed counts away from the source: down/right up, up/left down.
    let dir = if backwards { -1.0 } else { 1.0 };
    let lone_plain = len == 1 && matches!(seeds[0], Seed::Num(_));
    // Ctrl swaps a plain drag's copy and series: one plain number counts,
    // anything else is copied.
    let kind = match (kind, ctrl) {
        (FillKind::Auto, true) if lone_plain => FillKind::Series,
        (FillKind::Auto, true) => FillKind::Copy,
        (k, _) => k,
    };
    if matches!(kind, FillKind::Copy | FillKind::FormatsOnly) {
        return copy_all();
    }
    if lone_plain && matches!(kind, FillKind::Auto | FillKind::WithoutFormatting) {
        return copy_all();
    }
    // Each position's group: the positions sharing its key, in order.
    let keys: Vec<Option<Key>> = seeds.iter().map(Seed::key).collect();
    (0..count)
        .map(|t| {
            let k = t % len;
            let Some(key) = &keys[k] else {
                return LineOut::Copy(k);
            };
            let members: Vec<usize> = (0..len)
                .filter(|&j| keys[j].as_ref() == Some(key))
                .collect();
            let m = members.len();
            let i = members.iter().position(|&j| j == k).unwrap_or(0);
            let ys: Vec<f64> = members.iter().map(|&j| seeds[j].value()).collect();
            // This output's index on the group's own number line.
            let n = ((t / len + 1) * m + i) as f64;
            let seed = &seeds[k];
            let v = match seed {
                Seed::Date(_, tm) => date_at(&ys, n, *tm, kind, ctx.date1904, dir),
                Seed::Num(_) => match kind {
                    FillKind::GrowthTrend => growth_at(&ys, n),
                    _ => linear_at(&ys, n, dir),
                },
                _ => {
                    // Counted text steps by whole numbers: the last step.
                    let step = if m == 1 { dir } else { ys[m - 1] - ys[m - 2] };
                    ys[m - 1] + step * (n - (m - 1) as f64)
                }
            };
            LineOut::Value(render(seed, v, src[k].as_ref(), &lists))
        })
        .collect()
}

/// Where a drag of the fill handle from `src` to the cell `to` lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillTarget {
    /// Nothing changes: the handle is back where it started.
    None,
    /// A fill into `dest`, in `dir` (beside the source, along one axis).
    Extend { dir: FillDir, dest: Rect },
    /// The handle was dragged back inside the source: `cells` leave the
    /// selection and are cleared (contents only).
    Clear(Rect),
}

/// The direction a fill runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillDir {
    Down,
    Right,
    Up,
    Left,
}

impl FillDir {
    pub fn label(self) -> &'static str {
        match self {
            FillDir::Down => "Down",
            FillDir::Right => "Right",
            FillDir::Up => "Up",
            FillDir::Left => "Left",
        }
    }
}

impl FillTarget {
    /// The selection the gesture leaves: the source and the filled cells, or
    /// the source without the cleared ones.
    pub fn selection(self, src: Rect) -> Rect {
        let (r0, c0, r1, c1) = src;
        match self {
            FillTarget::None => src,
            FillTarget::Extend { dest, .. } => (
                r0.min(dest.0),
                c0.min(dest.1),
                r1.max(dest.2),
                c1.max(dest.3),
            ),
            FillTarget::Clear((cr0, cc0, _, _)) => {
                if cr0 > r0 {
                    (r0, c0, cr0 - 1, c1)
                } else {
                    (r0, c0, r1, cc0 - 1)
                }
            }
        }
    }
}

/// Where the fill handle of `src`, dragged to `to`, fills: along the axis
/// pulled furthest out of the source (down/up wins a tie, as before), or,
/// dragged back inside, the rows (or columns) it left behind.
pub fn fill_target(src: Rect, to: (u32, u32)) -> FillTarget {
    let (r0, c0, r1, c1) = src;
    if r0 > r1 || c0 > c1 {
        return FillTarget::None;
    }
    let (tr, tc) = to;
    let below = tr.saturating_sub(r1);
    let above = r0.saturating_sub(tr);
    let right = tc.saturating_sub(c1);
    let left = c0.saturating_sub(tc);
    let vertical = below.max(above);
    let horizontal = right.max(left);
    if vertical == 0 && horizontal == 0 {
        let rows = r1 - tr.clamp(r0, r1);
        let cols = c1 - tc.clamp(c0, c1);
        return if rows >= cols && rows > 0 {
            FillTarget::Clear((tr + 1, c0, r1, c1))
        } else if cols > 0 {
            FillTarget::Clear((r0, tc + 1, r1, c1))
        } else {
            FillTarget::None
        };
    }
    if vertical >= horizontal {
        if below > 0 {
            FillTarget::Extend {
                dir: FillDir::Down,
                dest: (r1 + 1, c0, tr, c1),
            }
        } else {
            FillTarget::Extend {
                dir: FillDir::Up,
                dest: (tr, c0, r0 - 1, c1),
            }
        }
    } else if right > 0 {
        FillTarget::Extend {
            dir: FillDir::Right,
            dest: (r0, c1 + 1, r1, tc),
        }
    } else {
        FillTarget::Extend {
            dir: FillDir::Left,
            dest: (r0, tc, r1, c0 - 1),
        }
    }
}

/// The Series dialog's Type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SeriesType {
    #[default]
    Linear,
    Growth,
    /// Date, by the unit (`Days`, `Weekdays`, `Months` or `Years`).
    Date(FillKind),
    AutoFill,
}

/// Home › Fill › Series…: what the dialog's fields say.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeriesSpec {
    /// Series in Rows (else Columns).
    pub rows: bool,
    pub kind: SeriesType,
    pub step: f64,
    pub stop: Option<f64>,
    /// Trend: replace the line's values with their least-squares fit,
    /// ignoring the step; a line with one seed steps by `step` from it.
    pub trend: bool,
}

impl Default for SeriesSpec {
    fn default() -> Self {
        SeriesSpec {
            rows: false,
            kind: SeriesType::Linear,
            step: 1.0,
            stop: None,
            trend: false,
        }
    }
}

/// The Series dialog's default orientation for a selection: rows when it is
/// wider than tall, as Excel guesses.
pub fn series_rows_for(rect: Rect) -> bool {
    let (r0, c0, r1, c1) = rect;
    (c1 - c0) > (r1 - r0)
}

/// The cells Home › Fill › Series writes over `rect` on a sheet, line by
/// line. Each line's leading non-empty cells are its seeds; the rest of the
/// line is filled from the first seed by the step (Linear adds it, Growth
/// multiplies by it, Date steps the unit), stopping once a value would pass
/// `stop`. With only one cell selected and a stop value, the series runs on
/// past the selection to the stop value. Trend replaces the whole line,
/// seeds included, with the least-squares fit of its seeds. AutoFill fills
/// the line as a drag of its seeds' fill handle would. Returns the
/// `(row, col, cell)` writes; a line with no seed writes nothing.
pub(crate) fn series_changes(
    sheet: &crate::sheet::Sheet,
    rect: Rect,
    spec: &SeriesSpec,
    ctx: &SeedCtx,
) -> Result<Vec<(u32, u32, Cell)>, &'static str> {
    use crate::sheet::{MAX_COLS, MAX_ROWS};
    let (r0, c0, r1, c1) = rect;
    let single = r0 == r1 && c0 == c1;
    // A step the arithmetic cannot take (#707 r2 M2).
    if !spec.step.is_finite() || spec.step.abs() > MAX_STEP {
        return Err(STEP_OUT_OF_RANGE);
    }
    // AutoFill fills the selection only; Trend fits it. The stop value
    // bounds the other types' series, and with one cell selected it is
    // what ends the series, so it has to be reachable (#707 r2 M1).
    let stop = spec
        .stop
        .filter(|_| spec.kind != SeriesType::AutoFill && !spec.trend);
    let mut out = Vec::new();
    let lines: Vec<u32> = if spec.rows {
        (r0..=r1).collect()
    } else {
        (c0..=c1).collect()
    };
    for line in lines {
        let at = |i: u32| -> (u32, u32) {
            if spec.rows {
                (line, c0 + i)
            } else {
                (r0 + i, line)
            }
        };
        let mut len = if spec.rows { c1 - c0 + 1 } else { r1 - r0 + 1 };
        if single && stop.is_some() {
            // One cell and a stop value: the series runs on to the stop
            // value.
            len = if spec.rows {
                MAX_COLS - c0
            } else {
                MAX_ROWS - r0
            };
        }
        // Capped as a paste is, whatever the selection (#707 r2 M2).
        len = len.min(super::MAX_PASTE_CELLS as u32);
        let seeds: Vec<Cell> = (0..len)
            .map_while(|i| {
                let (r, c) = at(i);
                sheet
                    .cell(r, c)
                    .filter(|c| !c.value.is_empty() || c.formula.is_some())
                    .cloned()
            })
            .collect();
        let Some(first) = seeds.first() else {
            continue;
        };
        if spec.kind == SeriesType::AutoFill {
            let src: Vec<Option<Cell>> = seeds.iter().cloned().map(Some).collect();
            let count = (len as usize).saturating_sub(src.len());
            for (k, o) in extend_line(&src, count, FillKind::Auto, false, false, ctx)
                .into_iter()
                .enumerate()
            {
                let i = (src.len() + k) as u32;
                let (r, c) = at(i);
                let cell = match o {
                    LineOut::Copy(j) => {
                        let mut cell = src[j].clone().unwrap_or_default();
                        let (sr, sc) = at(j as u32);
                        super::rebase(
                            &mut cell,
                            i64::from(r) - i64::from(sr),
                            i64::from(c) - i64::from(sc),
                        );
                        cell
                    }
                    LineOut::Value(cell) => cell,
                };
                out.push((r, c, cell));
            }
            continue;
        }
        let CellValue::Number(v0) = first.value else {
            continue;
        };
        if first.formula.is_some() {
            continue;
        }
        if let Some(stop) = stop {
            if single && !stop_reachable(spec, v0, stop) {
                return Err(STOP_UNREACHABLE);
            }
        }
        // A date series that would step off the calendar (#707 r3 m2).
        if let SeriesType::Date(unit) = spec.kind {
            if !spec.trend {
                let per = match unit {
                    FillKind::Weekdays => 1.4,
                    FillKind::Months => 31.0,
                    FillKind::Years => 366.0,
                    _ => 1.0,
                } * spec.step.abs();
                let mut steps = f64::from(len.saturating_sub(1));
                if let (true, Some(stop)) = (single, stop) {
                    steps = steps.min(((stop - v0).abs() / per.max(f64::MIN_POSITIVE)).ceil());
                }
                let end = v0 + spec.step.signum() * per * steps;
                if !in_date_range(v0) || !in_date_range(end) {
                    return Err(STEP_OUT_OF_RANGE);
                }
            }
        }
        let style = first.style;
        let ys: Vec<f64> = seeds
            .iter()
            .map_while(|c| match (&c.value, &c.formula) {
                (CellValue::Number(n), None) => Some(*n),
                _ => None,
            })
            .collect();
        // Whether `v` is past the stop value, in the series' direction.
        // Which way the series runs, as `stop_reachable` reads it too.
        let dir = series_dir(spec, v0);
        let past = |v: f64| match stop {
            Some(stop) if dir > 0.0 => v > stop,
            Some(stop) if dir < 0.0 => v < stop,
            _ => false,
        };
        let mut prev: Option<f64> = None;
        let start = if spec.trend { 0 } else { 1 };
        for i in start..len {
            let n = f64::from(i);
            let v = if spec.trend {
                match spec.kind {
                    SeriesType::Growth => growth_trend(&ys, n),
                    _ => {
                        let (a, b) = if ys.len() == 1 {
                            (ys[0], spec.step)
                        } else {
                            linear_fit(&ys)
                        };
                        a + b * n
                    }
                }
            } else {
                match spec.kind {
                    SeriesType::Linear => v0 + spec.step * n,
                    SeriesType::Growth => v0 * spec.step.powf(n),
                    SeriesType::Date(unit) => {
                        let k = (spec.step * n).round() as i64;
                        match unit {
                            FillKind::Weekdays => add_weekdays(v0, k, ctx.date1904),
                            FillKind::Months => add_months(v0, k, ctx.date1904),
                            FillKind::Years => add_months(v0, 12 * k, ctx.date1904),
                            _ => v0 + spec.step * n,
                        }
                    }
                    SeriesType::AutoFill => unreachable!(),
                }
            };
            // Past the stop; and, the safety net, a series that has stopped
            // moving never reaches it (#707 r3 M2).
            if past(v) || (stop.is_some() && prev == Some(v)) {
                break;
            }
            prev = Some(v);
            let (r, c) = at(i);
            let mut cell = Cell::number(v);
            cell.style = sheet
                .cell(r, c)
                .map_or(style, |c| if c.style == 0 { style } else { c.style });
            out.push((r, c, cell));
        }
    }
    Ok(out)
}

/// The largest step Series takes: past it a date step's whole-number
/// arithmetic would saturate.
const MAX_STEP: f64 = 1e12;

/// Series' refusal of a step it cannot take.
pub const STEP_OUT_OF_RANGE: &str = "The step value is out of range.";

/// Series' refusal of a stop value its step never reaches.
pub const STOP_UNREACHABLE: &str = "The stop value can never be reached with this step value.";

/// Which way a series from `v0` runs: +1 up, -1 down, 0 when it does not
/// move (or, for Growth with a step of 0 or below, does not run one way).
/// What both the stop check and `past` read (#707 r3 M1).
fn series_dir(spec: &SeriesSpec, v0: f64) -> f64 {
    let d = match spec.kind {
        SeriesType::Growth if spec.step <= 0.0 => return 0.0,
        SeriesType::Growth => v0 * spec.step - v0,
        _ => spec.step,
    };
    if d > 0.0 {
        1.0
    } else if d < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// Whether a series from `v0` stepping by `spec.step` ever reaches `stop`.
/// A series that does not move never does, even a stop equal to its seed
/// (#707 r3 M2); a growing or shrinking one only on its own side of the
/// seed, and a shrinking one never at or past zero.
fn stop_reachable(spec: &SeriesSpec, v0: f64, stop: f64) -> bool {
    let dir = series_dir(spec, v0);
    if dir == 0.0 {
        return false;
    }
    match spec.kind {
        SeriesType::Growth => {
            let ratio = stop / v0;
            ratio > 0.0
                && if spec.step > 1.0 {
                    ratio >= 1.0
                } else {
                    ratio <= 1.0
                }
        }
        _ => (stop - v0) * dir >= 0.0,
    }
}

/// The growth trend's value at `n`: the least-squares exponential through
/// the seeds, or the seed itself when there is only one.
fn growth_trend(ys: &[f64], n: f64) -> f64 {
    if ys.len() == 1 {
        return ys[0];
    }
    if ys.iter().all(|&y| y > 0.0) {
        let logs: Vec<f64> = ys.iter().map(|y| y.ln()).collect();
        let (a, b) = linear_fit(&logs);
        return (a + b * n).exp();
    }
    let (a, b) = linear_fit(ys);
    a + b * n
}

/// Where a double-click on the fill handle of `src` fills down to: the last
/// row of the block in the neighbouring column (left first, then right)
/// that runs on below the source. `None` when neither neighbour runs on.
pub fn fill_down_to(sheet: &crate::sheet::Sheet, src: Rect) -> Option<u32> {
    let (_, c0, r1, c1) = src;
    let filled = |r: u32, c: u32| {
        sheet
            .cell(r, c)
            .is_some_and(|x| !x.value.is_empty() || x.formula.is_some())
    };
    let mut cols = Vec::new();
    if c0 > 0 {
        cols.push(c0 - 1);
    }
    cols.push(c1 + 1);
    cols.into_iter().find_map(|c| {
        let mut end = r1;
        while end + 1 < crate::sheet::MAX_ROWS && filled(end + 1, c) {
            end += 1;
        }
        (end > r1).then_some(end)
    })
}

/// Excel's question when Home › Fill › Justify needs more rows than the
/// selection has.
pub const JUSTIFY_OVERFLOW: &str = "Text will extend below selected range.";

/// Home › Fill › Justify's lines: the words of `texts`, joined, rewrapped to
/// `width` characters a line (at least one word a line, so a word longer
/// than the width stands alone).
pub fn justify_lines(texts: &[String], width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in texts.iter().flat_map(|t| t.split_whitespace()) {
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + word.chars().count() <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
#[path = "series/tests.rs"]
mod tests;
