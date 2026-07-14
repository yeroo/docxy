//! The number-format runtime: real rendering of Excel format codes.
//!
//! This is what powers `TEXT()` and cell display beyond the coarse
//! classification in [`crate::sheet::NumFmt`]. A format code is parsed once
//! into sections and tokens, then rendered against a value.
//!
//! Honesty contract: [`parse_format`] returns `None` for constructs we don't
//! model (fractions, locale-dependent day names beyond English, etc.), and
//! callers fall back — `TEXT()` marks itself unsupported rather than
//! fabricating output, and cell display falls back to the classified
//! approximation.

use crate::sheet::{fmt_general, serial_to_parts};

/// One digit-placeholder or literal token of a numeric section.
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// `0` (forced), `#` (optional), `?` (space-padded) — position matters.
    Digit(char),
    /// The decimal point.
    Point,
    /// Literal text (from quotes, escapes, or pass-through characters).
    Lit(String),
    /// `%` — scale ×100 and print the sign.
    Percent,
    /// `E+`/`E-` followed by exponent digit placeholders.
    Exp {
        plus: bool,
        digits: usize,
    },
    /// `@` — the raw text value (text sections).
    TextValue,
    /// `General` — render with the General algorithm.
    General,
    // --- date/time tokens ---
    Year(usize),   // yy / yyyy
    Month(usize),  // m / mm / mmm / mmmm
    Day(usize),    // d / dd / ddd / dddd
    Hour(usize),   // h / hh
    Minute(usize), // m / mm (disambiguated at parse time)
    Second(usize), // s / ss
    /// `[h]` / `[m]` / `[s]` — elapsed totals.
    Elapsed(char),
    /// AM/PM (switches hours to 12-hour clock).
    AmPm,
}

/// A `;`-separated section: its tokens plus precomputed numeric facts.
#[derive(Clone, Debug)]
struct Section {
    toks: Vec<Tok>,
    /// A `[<100]`-style condition overriding positional selection.
    condition: Option<(CmpOp, f64)>,
    /// Number of integer / decimal digit placeholders.
    int_digits: usize,
    dec_digits: usize,
    /// Thousands grouping active (a `,` between digit placeholders).
    grouping: bool,
    /// Each trailing `,` after the digits divides by 1000.
    scale_commas: u32,
    /// Contains any date/time token.
    is_date: bool,
    /// Contains `@` (text section) .
    is_text: bool,
    has_percent: bool,
    exp: Option<(bool, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum CmpOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// A parsed, renderable number format.
#[derive(Clone, Debug)]
pub struct NumFormat {
    sections: Vec<Section>,
}

/// Parse a format code. `None` = something we can't honestly render.
pub fn parse_format(code: &str) -> Option<NumFormat> {
    let mut sections = Vec::new();
    for part in split_sections(code) {
        sections.push(parse_section(&part)?);
    }
    if sections.is_empty() || sections.len() > 4 {
        return None;
    }
    Some(NumFormat { sections })
}

/// Split on `;` outside quotes/brackets/escapes.
fn split_sections(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = code.chars().peekable();
    let mut in_quote = false;
    let mut in_bracket = false;
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                cur.push(ch);
            }
            '[' if !in_quote => {
                in_bracket = true;
                cur.push(ch);
            }
            ']' if !in_quote => {
                in_bracket = false;
                cur.push(ch);
            }
            '\\' if !in_quote => {
                cur.push(ch);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ';' if !in_quote && !in_bracket => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

fn parse_section(src: &str) -> Option<Section> {
    let mut toks: Vec<Tok> = Vec::new();
    let mut condition = None;
    let mut chars: Vec<char> = src.chars().collect();

    // General (alone, case-insensitive) is its own beast.
    if src.trim().eq_ignore_ascii_case("general") {
        return Some(Section {
            toks: vec![Tok::General],
            condition: None,
            int_digits: 0,
            dec_digits: 0,
            grouping: false,
            scale_commas: 0,
            is_date: false,
            is_text: false,
            has_percent: false,
            exp: None,
        });
    }

    let mut i = 0;
    let mut last_time_tok: Option<char> = None; // 'h' or 's' context for m
    // Normalize to lowercase where case doesn't matter.
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                let mut lit = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    lit.push(chars[i]);
                    i += 1;
                }
                push_lit(&mut toks, &lit);
            }
            '\\' => {
                if i + 1 < chars.len() {
                    push_lit(&mut toks, &chars[i + 1].to_string());
                    i += 1;
                }
            }
            '_' => {
                // Width-of-next-char skip → a space.
                push_lit(&mut toks, " ");
                i += 1;
            }
            '*' => {
                // Fill: unbounded repetition makes no sense off-grid; skip it
                // and the fill character.
                i += 1;
            }
            '[' => {
                let start = i + 1;
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
                let inner: String = chars[start..i.min(chars.len())].iter().collect();
                let lower = inner.to_ascii_lowercase();
                match lower.as_str() {
                    "h" | "hh" => {
                        toks.push(Tok::Elapsed('h'));
                        last_time_tok = Some('h');
                    }
                    "m" | "mm" => toks.push(Tok::Elapsed('m')),
                    "s" | "ss" => toks.push(Tok::Elapsed('s')),
                    _ if lower.starts_with('$') => {
                        // [$€-407]: currency symbol + locale id. Emit the
                        // symbol part, drop the locale.
                        let sym = inner[1..].split('-').next().unwrap_or("");
                        if !sym.is_empty() {
                            push_lit(&mut toks, sym);
                        }
                    }
                    // Colors: display-only concern, no textual output.
                    "black" | "blue" | "cyan" | "green" | "magenta" | "red" | "white"
                    | "yellow" => {}
                    _ if lower.starts_with("color") => {}
                    _ => {
                        // A condition like [<100] / [>=1e3]?
                        let cond = parse_condition(&inner)?;
                        condition = Some(cond);
                    }
                }
            }
            '0' | '#' | '?' => toks.push(Tok::Digit(c)),
            '.' => toks.push(Tok::Point),
            ',' => toks.push(Tok::Lit(",".into())), // classified in analyze()
            '%' => toks.push(Tok::Percent),
            '@' => toks.push(Tok::TextValue),
            'e' | 'E' => {
                if i + 1 < chars.len() && (chars[i + 1] == '+' || chars[i + 1] == '-') {
                    let plus = chars[i + 1] == '+';
                    i += 2;
                    let mut digits = 0;
                    while i < chars.len() && matches!(chars[i], '0' | '#' | '?') {
                        digits += 1;
                        i += 1;
                    }
                    i -= 1;
                    toks.push(Tok::Exp {
                        plus,
                        digits: digits.max(1),
                    });
                } else {
                    push_lit(&mut toks, "E");
                }
            }
            'g' | 'G' => {
                // "General" embedded mid-section — treat like the keyword.
                let rest: String = chars[i..].iter().collect();
                if rest.to_ascii_lowercase().starts_with("general") {
                    toks.push(Tok::General);
                    i += "general".len() - 1;
                } else {
                    return None; // unknown letter
                }
            }
            'y' | 'Y' => {
                let n = run_len(&chars, i, |x| x.eq_ignore_ascii_case(&'y'));
                toks.push(Tok::Year(if n >= 4 { 4 } else { 2 }));
                i += n - 1;
            }
            'd' | 'D' => {
                let n = run_len(&chars, i, |x| x.eq_ignore_ascii_case(&'d'));
                if n > 4 {
                    return None;
                }
                toks.push(Tok::Day(n));
                i += n - 1;
            }
            'h' | 'H' => {
                let n = run_len(&chars, i, |x| x.eq_ignore_ascii_case(&'h'));
                toks.push(Tok::Hour(n.min(2)));
                last_time_tok = Some('h');
                i += n - 1;
            }
            's' | 'S' => {
                let n = run_len(&chars, i, |x| x.eq_ignore_ascii_case(&'s'));
                // Retroactively fix `m` before `s` to minutes.
                fix_prev_month_to_minute(&mut toks);
                toks.push(Tok::Second(n.min(2)));
                last_time_tok = Some('s');
                i += n - 1;
            }
            'm' | 'M' => {
                let n = run_len(&chars, i, |x| x.eq_ignore_ascii_case(&'m'));
                if last_time_tok == Some('h') {
                    toks.push(Tok::Minute(n.min(2)));
                } else if n > 5 {
                    return None;
                } else {
                    toks.push(Tok::Month(n));
                }
                i += n - 1;
            }
            'a' | 'A' => {
                let rest: String = chars[i..].iter().collect::<String>().to_ascii_lowercase();
                if rest.starts_with("am/pm") {
                    toks.push(Tok::AmPm);
                    i += 4;
                } else if rest.starts_with("a/p") {
                    toks.push(Tok::AmPm);
                    i += 2;
                } else {
                    return None;
                }
            }
            // Common literal punctuation passes through.
            ' ' | '-' | '+' | '/' | '(' | ')' | ':' | '$' | '!' | '^' | '&' | '\'' | '~' | '{'
            | '}' | '<' | '>' | '=' => push_lit(&mut toks, &c.to_string()),
            _ => return None, // anything else: be honest, refuse
        }
        i += 1;
    }
    let _ = &mut chars;

    analyze(toks, condition)
}

fn push_lit(toks: &mut Vec<Tok>, s: &str) {
    if s.is_empty() {
        return;
    }
    if let Some(Tok::Lit(prev)) = toks.last_mut() {
        prev.push_str(s);
    } else {
        toks.push(Tok::Lit(s.to_string()));
    }
}

fn run_len(chars: &[char], at: usize, pred: impl Fn(&char) -> bool) -> usize {
    chars[at..].iter().take_while(|c| pred(c)).count()
}

/// `m` before `s` means minutes: patch the most recent Month token.
fn fix_prev_month_to_minute(toks: &mut [Tok]) {
    for t in toks.iter_mut().rev() {
        match t {
            Tok::Month(n) if *n <= 2 => {
                *t = Tok::Minute(*n);
                return;
            }
            Tok::Year(_) | Tok::Day(_) | Tok::Hour(_) | Tok::Minute(_) | Tok::Second(_) => {
                return;
            }
            _ => {}
        }
    }
}

fn parse_condition(s: &str) -> Option<(CmpOp, f64)> {
    let s = s.trim();
    let (op, rest) = if let Some(r) = s.strip_prefix(">=") {
        (CmpOp::Ge, r)
    } else if let Some(r) = s.strip_prefix("<=") {
        (CmpOp::Le, r)
    } else if let Some(r) = s.strip_prefix("<>") {
        (CmpOp::Ne, r)
    } else if let Some(r) = s.strip_prefix('<') {
        (CmpOp::Lt, r)
    } else if let Some(r) = s.strip_prefix('>') {
        (CmpOp::Gt, r)
    } else {
        (CmpOp::Eq, s.strip_prefix('=')?)
    };
    rest.trim().parse::<f64>().ok().map(|v| (op, v))
}

/// Post-parse analysis: classify commas, count placeholders.
fn analyze(mut toks: Vec<Tok>, condition: Option<(CmpOp, f64)>) -> Option<Section> {
    let is_date = toks.iter().any(|t| {
        matches!(
            t,
            Tok::Year(_)
                | Tok::Month(_)
                | Tok::Day(_)
                | Tok::Hour(_)
                | Tok::Minute(_)
                | Tok::Second(_)
                | Tok::Elapsed(_)
                | Tok::AmPm
        )
    });
    let is_text = toks.iter().any(|t| matches!(t, Tok::TextValue));
    let has_percent = toks.iter().any(|t| matches!(t, Tok::Percent));
    let mut exp = None;
    for t in &toks {
        if let Tok::Exp { plus, digits } = t {
            exp = Some((*plus, *digits));
        }
    }
    if is_date && (has_percent || exp.is_some()) {
        return None;
    }
    // Fraction formats ("# ?/?") aren't modeled — refuse rather than guess.
    if !is_date
        && toks.iter().any(|t| matches!(t, Tok::Digit('?')))
        && toks
            .iter()
            .any(|t| matches!(t, Tok::Lit(s) if s.contains('/')))
    {
        return None;
    }

    // Comma classification: a comma between digit placeholders → grouping;
    // commas immediately after the last digit placeholder → scaling; commas
    // elsewhere → literal (already Lit(",")).
    let digit_positions: Vec<usize> = toks
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(t, Tok::Digit(_)))
        .map(|(i, _)| i)
        .collect();
    let mut grouping = false;
    let mut scale_commas = 0u32;
    if !is_date && !digit_positions.is_empty() {
        let first = digit_positions[0];
        let last = *digit_positions.last().unwrap();
        // Point position (if any) bounds the integer part.
        let point = toks.iter().position(|t| matches!(t, Tok::Point));
        let int_last = match point {
            Some(p) => digit_positions.iter().copied().rfind(|&i| i < p),
            None => Some(last),
        };
        let _ = int_last;
        let mut drop: Vec<usize> = Vec::new();
        for (i, t) in toks.iter().enumerate() {
            if let Tok::Lit(s) = t {
                if !s.chars().all(|c| c == ',') {
                    continue;
                }
                if i > first && i < last && point.map(|p| i < p).unwrap_or(true) {
                    // Between integer digit placeholders → grouping.
                    grouping = true;
                    drop.push(i);
                } else if i > last {
                    // Immediately after the last digit placeholder → each
                    // comma scales by a thousand ("0.0,," = millions).
                    if toks[last + 1..i]
                        .iter()
                        .all(|x| matches!(x, Tok::Lit(s2) if s2.chars().all(|c| c == ',')))
                    {
                        scale_commas += s.len() as u32;
                        drop.push(i);
                    }
                }
            }
        }
        for &i in drop.iter().rev() {
            toks.remove(i);
        }
    }

    // Placeholder counts.
    let point = toks.iter().position(|t| matches!(t, Tok::Point));
    let mut int_digits = 0;
    let mut dec_digits = 0;
    for (i, t) in toks.iter().enumerate() {
        if matches!(t, Tok::Digit(_)) {
            // Exponent digits are counted inside Tok::Exp, not here.
            match point {
                Some(p) if i > p => dec_digits += 1,
                _ => int_digits += 1,
            }
        }
    }

    Some(Section {
        toks,
        condition,
        int_digits,
        dec_digits,
        grouping,
        scale_commas,
        is_date,
        is_text,
        has_percent,
        exp,
    })
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl NumFormat {
    /// Render a number. `None` = this format can't render numbers (pure-text
    /// section set) — the caller falls back.
    pub fn format_number(&self, v: f64, date1904: bool) -> Option<String> {
        if !v.is_finite() {
            return None;
        }
        let section = self.pick_section(v)?;
        // Negative sections format the absolute value (the "-" is the
        // section's own business, e.g. parentheses accounting style)…
        let explicit = self.sections.len() >= 2 || section.condition.is_some();
        let x = if explicit { v.abs() } else { v };
        Some(render_section(section, x, v, explicit, date1904))
    }

    /// Render a text value (through the `@` section when present).
    pub fn format_text(&self, s: &str) -> String {
        // The 4th section (or any section with @) formats text.
        let sect = self
            .sections
            .iter()
            .find(|x| x.is_text)
            .or(if self.sections.len() == 4 {
                self.sections.get(3)
            } else {
                None
            });
        match sect {
            None => s.to_string(),
            Some(sect) => {
                let mut out = String::new();
                for t in &sect.toks {
                    match t {
                        Tok::TextValue => out.push_str(s),
                        Tok::Lit(l) => out.push_str(l),
                        Tok::General => out.push_str(s),
                        _ => {}
                    }
                }
                out
            }
        }
    }

    /// Is this format date-flavored (drives right-alignment etc.)?
    pub fn is_date(&self) -> bool {
        self.sections.first().is_some_and(|s| s.is_date)
    }

    fn pick_section(&self, v: f64) -> Option<&Section> {
        let numeric: Vec<&Section> = self.sections.iter().filter(|s| !s.is_text).collect();
        if numeric.is_empty() {
            return None;
        }
        // Conditions first.
        let has_conditions = numeric.iter().any(|s| s.condition.is_some());
        if has_conditions {
            for s in &numeric {
                if let Some((op, bound)) = s.condition {
                    let hit = match op {
                        CmpOp::Lt => v < bound,
                        CmpOp::Le => v <= bound,
                        CmpOp::Gt => v > bound,
                        CmpOp::Ge => v >= bound,
                        CmpOp::Eq => v == bound,
                        CmpOp::Ne => v != bound,
                    };
                    if hit {
                        return Some(s);
                    }
                }
            }
            // Fall through to the first unconditioned section.
            return numeric.iter().find(|s| s.condition.is_none()).copied();
        }
        match numeric.len() {
            1 => Some(numeric[0]),
            2 => Some(if v < 0.0 { numeric[1] } else { numeric[0] }),
            _ => Some(if v > 0.0 {
                numeric[0]
            } else if v < 0.0 {
                numeric[1]
            } else {
                numeric[2]
            }),
        }
    }
}

fn render_section(
    sect: &Section,
    x: f64,
    original: f64,
    explicit_sign: bool,
    date1904: bool,
) -> String {
    if sect.toks.iter().any(|t| matches!(t, Tok::General)) {
        let mut out = String::new();
        for t in &sect.toks {
            match t {
                Tok::General => out.push_str(&fmt_general(original)),
                Tok::Lit(l) => out.push_str(l),
                _ => {}
            }
        }
        return out;
    }
    if sect.is_date {
        return render_date(sect, x, date1904);
    }
    render_number(sect, x, explicit_sign)
}

fn render_date(sect: &Section, serial: f64, date1904: bool) -> String {
    let Some(p) = serial_to_parts(serial, date1904) else {
        return fmt_general(serial);
    };
    let twelve_hour = sect.toks.iter().any(|t| matches!(t, Tok::AmPm));
    let mut out = String::new();
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    // Day-of-week from the serial: Excel's WEEKDAY convention has serial 1
    // as a "Sunday", so Sunday-index = (serial - 1) mod 7.
    let dow = (serial.floor() as i64 - 1).rem_euclid(7);
    const DAYS: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    for t in &sect.toks {
        match t {
            Tok::Lit(l) => out.push_str(l),
            Tok::Year(4) => out.push_str(&format!("{:04}", p.year)),
            Tok::Year(_) => out.push_str(&format!("{:02}", p.year.rem_euclid(100))),
            Tok::Month(1) => out.push_str(&p.month.to_string()),
            Tok::Month(2) => out.push_str(&format!("{:02}", p.month)),
            Tok::Month(3) => out.push_str(&MONTHS[p.month as usize - 1][..3]),
            Tok::Month(4) => out.push_str(MONTHS[p.month as usize - 1]),
            Tok::Month(_) => out.push(MONTHS[p.month as usize - 1].chars().next().unwrap()),
            Tok::Day(1) => out.push_str(&p.day.to_string()),
            Tok::Day(2) => out.push_str(&format!("{:02}", p.day)),
            Tok::Day(3) => out.push_str(&DAYS[dow as usize][..3]),
            Tok::Day(_) => out.push_str(DAYS[dow as usize]),
            Tok::Hour(n) => {
                let mut h = p.hour;
                if twelve_hour {
                    h %= 12;
                    if h == 0 {
                        h = 12;
                    }
                }
                if *n >= 2 {
                    out.push_str(&format!("{h:02}"));
                } else {
                    out.push_str(&h.to_string());
                }
            }
            Tok::Minute(n) => {
                if *n >= 2 {
                    out.push_str(&format!("{:02}", p.minute));
                } else {
                    out.push_str(&p.minute.to_string());
                }
            }
            Tok::Second(n) => {
                if *n >= 2 {
                    out.push_str(&format!("{:02}", p.second));
                } else {
                    out.push_str(&p.second.to_string());
                }
            }
            Tok::Elapsed('h') => {
                out.push_str(&((serial * 24.0).floor() as i64).to_string());
            }
            Tok::Elapsed('m') => {
                out.push_str(&((serial * 24.0 * 60.0).floor() as i64).to_string());
            }
            Tok::Elapsed(_) => {
                out.push_str(&((serial * 86_400.0).round() as i64).to_string());
            }
            Tok::AmPm => out.push_str(if p.hour < 12 { "AM" } else { "PM" }),
            _ => {}
        }
    }
    out
}

fn render_number(sect: &Section, x: f64, explicit_sign: bool) -> String {
    let mut v = x;
    if sect.has_percent {
        v *= 100.0;
    }
    for _ in 0..sect.scale_commas {
        v /= 1000.0;
    }

    // Scientific: v = mantissa * 10^exp with int_digits mantissa digits.
    let mut exp_val: i32 = 0;
    if let Some((_, _)) = sect.exp {
        if v != 0.0 {
            let want = sect.int_digits.max(1) as i32;
            let mag = v.abs().log10().floor() as i32;
            exp_val = mag - (want - 1);
            v /= 10f64.powi(exp_val);
        }
    }

    let negative = v < 0.0 || (explicit_sign && x < 0.0);
    let rounded = {
        let f = 10f64.powi(sect.dec_digits as i32);
        (v.abs() * f).round() / f
    };
    let int_part = rounded.trunc() as u64;
    let frac = rounded.fract();

    // Integer digits, distributed right-to-left over placeholders. With a
    // zero integer part, only a forcing placeholder (0 or ?) prints anything:
    // "#.##" renders 0.5 as ".5", "0.##" as "0.5".
    let point_pos = sect.toks.iter().position(|t| matches!(t, Tok::Point));
    let int_forced = sect.toks.iter().enumerate().any(|(i, t)| {
        matches!(t, Tok::Digit('0') | Tok::Digit('?')) && point_pos.map(|p| i < p).unwrap_or(true)
    });
    let int_str = if int_part == 0 && !int_forced {
        String::new()
    } else {
        int_part.to_string()
    };
    let grouped = |s: &str| -> String {
        if !sect.grouping {
            return s.to_string();
        }
        let b = s.as_bytes();
        let mut out = String::new();
        for (i, ch) in b.iter().enumerate() {
            if i > 0 && (b.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(*ch as char);
        }
        out
    };

    // Fraction digits, left-to-right.
    let mut frac_digits = Vec::new();
    if sect.dec_digits > 0 {
        let scaled = (frac * 10f64.powi(sect.dec_digits as i32)).round() as u64;
        let s = format!("{:0>width$}", scaled, width = sect.dec_digits);
        frac_digits = s.chars().collect();
    }

    // Walk tokens, emitting digits into placeholders.
    let mut out = String::new();
    let mut int_emitted = false;
    let mut after_point = false;
    let mut dec_index = 0usize;
    // How many integer placeholders remain at each step (to know when to dump
    // the full integer prefix).
    let mut int_placeholders_left = sect.int_digits;
    for t in &sect.toks {
        match t {
            Tok::Digit(kind) => {
                if after_point {
                    let d = frac_digits.get(dec_index).copied();
                    dec_index += 1;
                    match (d, kind) {
                        (Some(d), _) => {
                            // Trailing '#' drops trailing zeros.
                            if *kind == '#'
                                && d == '0'
                                && frac_digits[dec_index..].iter().all(|&z| z == '0')
                            {
                                // drop
                            } else {
                                out.push(d);
                            }
                        }
                        (None, '0') => out.push('0'),
                        (None, '?') => out.push(' '),
                        _ => {}
                    }
                } else {
                    // Integer side.
                    if !int_emitted {
                        // This is the first integer placeholder: emit every
                        // digit that overflows the remaining placeholders,
                        // then this placeholder's own digit.
                        let take = int_str.len().saturating_sub(int_placeholders_left - 1);
                        if take > 0 {
                            out.push_str(
                                &grouped(&int_str)[..grouped_prefix_len(&grouped(&int_str), take)],
                            );
                        } else {
                            match kind {
                                '0' => out.push('0'),
                                '?' => out.push(' '),
                                _ => {}
                            }
                        }
                        int_emitted = true;
                        int_placeholders_left -= 1;
                    } else {
                        // Subsequent placeholders: one digit each from where
                        // the prefix left off.
                        let pos = int_str.len() as i64 - int_placeholders_left as i64;
                        if pos >= 0 {
                            let g = grouped(&int_str);
                            let gpos = grouped_prefix_len(&g, pos as usize);
                            let next = grouped_prefix_len(&g, pos as usize + 1);
                            out.push_str(&g[gpos..next]);
                        } else {
                            match kind {
                                '0' => out.push('0'),
                                '?' => out.push(' '),
                                _ => {}
                            }
                        }
                        int_placeholders_left -= 1;
                    }
                }
            }
            Tok::Point => {
                // Suppress a dangling point when no decimals will print
                // ("0.##" with 5 → "5."? Excel prints "5." actually; keep it).
                out.push('.');
                after_point = true;
            }
            Tok::Lit(l) => out.push_str(l),
            Tok::Percent => out.push('%'),
            Tok::Exp { plus, digits } => {
                out.push('E');
                if exp_val >= 0 {
                    if *plus {
                        out.push('+');
                    }
                } else {
                    out.push('-');
                }
                out.push_str(&format!("{:0>width$}", exp_val.abs(), width = digits));
            }
            _ => {}
        }
    }
    if negative { format!("-{out}") } else { out }
}

/// Byte length of the prefix of a grouped string covering `digits` digits.
fn grouped_prefix_len(grouped: &str, digits: usize) -> usize {
    let mut seen = 0;
    for (i, ch) in grouped.char_indices() {
        if seen == digits && ch != ',' {
            return i;
        }
        if ch != ',' {
            seen += 1;
        }
    }
    grouped.len()
}

/// The canonical code strings for builtin numFmtIds (ECMA-376 §18.8.30) —
/// lets files that use builtins render through the same runtime.
pub fn builtin_code(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        14 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yyyy h:mm",
        37 => "#,##0;(#,##0)",
        38 => "#,##0;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(code: &str, v: f64) -> String {
        parse_format(code)
            .unwrap_or_else(|| panic!("parse {code}"))
            .format_number(v, false)
            .unwrap_or_else(|| panic!("render {code}"))
    }

    #[test]
    fn plain_and_grouped_numbers() {
        assert_eq!(fmt("0", 5.0), "5");
        assert_eq!(fmt("0", 5.6), "6");
        assert_eq!(fmt("0.00", 5.0), "5.00");
        assert_eq!(fmt("0.00", -3.15159), "-3.15");
    }

    #[test]
    fn grouped_exact() {
        assert_eq!(fmt("#,##0", 1234567.0), "1,234,567");
        assert_eq!(fmt("#,##0.00", 1234.5), "1,234.50");
        assert_eq!(fmt("#,##0", 12.0), "12");
        assert_eq!(fmt("#,##0", 0.0), "0");
    }

    #[test]
    fn placeholders() {
        assert_eq!(fmt("00000", 42.0), "00042");
        assert_eq!(fmt("#.##", 5.0), "5.");
        assert_eq!(fmt("#.##", 5.25), "5.25");
        assert_eq!(fmt("#.##", 5.2), "5.2");
        assert_eq!(fmt("0.0#", 5.25), "5.25");
        assert_eq!(fmt("0.0#", 5.2), "5.2");
        assert_eq!(fmt("#.##", 0.5), ".5");
        assert_eq!(fmt("0.##", 0.5), "0.5");
    }

    #[test]
    fn percent_and_scaling() {
        assert_eq!(fmt("0%", 0.29), "29%");
        assert_eq!(fmt("0.0%", 0.285), "28.5%");
        assert_eq!(fmt("#,##0,", 12_200_000.0), "12,200");
        assert_eq!(fmt("0.0,,", 12_200_000.0), "12.2");
    }

    #[test]
    fn sections_and_conditions() {
        assert_eq!(fmt("0.00;(0.00)", -3.5), "(3.50)");
        assert_eq!(fmt("0.00;(0.00)", 3.5), "3.50");
        assert_eq!(fmt("0;-0;\"zero\"", 0.0), "zero");
        assert_eq!(fmt("0;[Red](0)", -7.0), "(7)");
        assert_eq!(fmt("[<10]\"small\";\"big\"", 5.0), "small");
        assert_eq!(fmt("[<10]\"small\";\"big\"", 50.0), "big");
    }

    #[test]
    fn currency_and_literals() {
        assert_eq!(fmt("$#,##0.00", 1234.5), "$1,234.50");
        assert_eq!(fmt("0.0\"kg\"", 3.25), "3.3kg");
        assert_eq!(fmt("\"~\"0", 5.0), "~5");
        assert_eq!(fmt("[$€-407]#,##0.00", 9.5), "€9.50");
        assert_eq!(fmt("0.00_)", 1.0), "1.00 ");
    }

    #[test]
    fn scientific() {
        assert_eq!(fmt("0.00E+00", 12345.0), "1.23E+04");
        assert_eq!(fmt("0.00E+00", 0.00123), "1.23E-03");
        assert_eq!(fmt("0.00E+00", 0.0), "0.00E+00");
    }

    #[test]
    fn dates_and_times() {
        // 45306 = 2024-01-15 (a Monday), plus 18:30:05.
        let serial = 45306.0 + (18.0 * 3600.0 + 30.0 * 60.0 + 5.0) / 86400.0;
        assert_eq!(fmt("yyyy-mm-dd", serial), "2024-01-15");
        assert_eq!(fmt("m/d/yyyy", serial), "1/15/2024");
        assert_eq!(fmt("d-mmm-yy", serial), "15-Jan-24");
        assert_eq!(fmt("dddd", serial), "Monday");
        assert_eq!(fmt("ddd", serial), "Mon");
        assert_eq!(fmt("mmmm yyyy", serial), "January 2024");
        assert_eq!(fmt("hh:mm:ss", serial), "18:30:05");
        assert_eq!(fmt("h:mm AM/PM", serial), "6:30 PM");
        assert_eq!(fmt("yyyy-mm-dd hh:mm", serial), "2024-01-15 18:30");
        // m as minutes after h; as month otherwise; before s → minutes.
        assert_eq!(fmt("h:m", serial), "18:30");
        assert_eq!(fmt("mm:ss", 0.5 + 305.0 / 86400.0), "05:05");
        // Elapsed hours beyond 24.
        assert_eq!(fmt("[h]:mm", 1.5), "36:00");
        assert_eq!(fmt("[mm]:ss", 0.25), "360:00");
    }

    #[test]
    fn text_sections() {
        let f = parse_format("0.00;-0.00;0;\"Item: \"@").unwrap();
        assert_eq!(f.format_text("pen"), "Item: pen");
        let f = parse_format("@").unwrap();
        assert_eq!(f.format_text("x"), "x");
    }

    #[test]
    fn refusals() {
        assert!(parse_format("# ?/?").is_none()); // fractions
        assert!(parse_format("0;0;0;0;0").is_none()); // too many sections
        assert!(parse_format("0\u{4e2d}0").is_none()); // opaque letter
    }

    #[test]
    fn builtins_render() {
        for id in [0, 1, 2, 3, 4, 9, 10, 11, 14, 18, 22, 37, 38, 45, 46, 49] {
            let code = builtin_code(id).unwrap();
            assert!(parse_format(code).is_some(), "builtin {id} = {code}");
        }
        assert_eq!(fmt(builtin_code(4).unwrap(), 1234.5), "1,234.50");
        assert_eq!(fmt(builtin_code(14).unwrap(), 45306.0), "1/15/2024");
    }
}
