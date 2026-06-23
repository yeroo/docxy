//! Evaluate computable Word fields, so docxy can show a live value instead of the
//! value Word last cached. Currently the `=` (formula) field — a self-contained
//! arithmetic expression — plus the result-formatting switches (`\#` numeric
//! picture and `\*` number/text format) that the field grammar shares. Field types
//! we don't compute return `None`, and the caller keeps the cached result.
//!
//! See ECMA-376 Part 1 §17.16.5 (field-instruction grammar) and §17.16.6 (the
//! field types and their switches).

/// Compute a field's result from its (entity-decoded) instruction, or `None` when
/// it isn't a field we evaluate (the caller keeps Word's cached result).
pub fn eval_field(instr: &str) -> Option<String> {
    let s = instr.trim();
    // The formula field's name is the literal `=`.
    if let Some(rest) = s.strip_prefix('=') {
        return eval_formula_field(rest);
    }
    let name_end = s.find(char::is_whitespace).unwrap_or(s.len());
    let name = s[..name_end].to_ascii_uppercase();
    let rest = &s[name_end..];
    match name.as_str() {
        "IF" => eval_if(rest),
        _ => None,
    }
}

/// `= Formula [\# Picture] [\* Format]` — evaluate the expression and format it.
fn eval_formula_field(rest: &str) -> Option<String> {
    let (expr, switches) = split_switches(rest);
    let value = eval_formula(&expr)?;
    let mut out = match switches.iter().find(|(c, _)| *c == '#') {
        Some((_, pic)) => format_numeric(value, pic),
        None => default_number(value),
    };
    if let Some((_, fmt)) = switches.iter().find(|(c, _)| *c == '*') {
        out = apply_star(&out, value, fmt);
    }
    Some(out)
}

/// `IF Expr1 Operator Expr2 TrueText FalseText` — compare two operands (numeric
/// when both are numbers, else string) and pick the matching text. Returns `None`
/// if an operand isn't a literal (e.g. a nested field), so the cache stands.
fn eval_if(rest: &str) -> Option<String> {
    let (head, _) = split_switches(rest);
    let args = field_args(&head);
    if args.len() < 5 {
        return None;
    }
    let cmp = |op: &str| -> Option<bool> {
        let (a, b) = (&args[0], &args[2]);
        Some(match (a.parse::<f64>(), b.parse::<f64>()) {
            (Ok(x), Ok(y)) => match op {
                "=" => x == y,
                "<>" => x != y,
                "<" => x < y,
                "<=" => x <= y,
                ">" => x > y,
                ">=" => x >= y,
                _ => return None,
            },
            _ => match op {
                "=" => a == b,
                "<>" => a != b,
                "<" => a < b,
                "<=" => a <= b,
                ">" => a > b,
                ">=" => a >= b,
                _ => return None,
            },
        })
    };
    Some(if cmp(&args[1])? {
        args[3].clone()
    } else {
        args[4].clone()
    })
}

/// Split a field's argument string into whitespace-separated tokens, keeping
/// `"quoted strings"` (with the quotes removed) as single tokens.
fn field_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut started = false;
    for c in s.chars() {
        if in_quote {
            if c == '"' {
                in_quote = false;
                out.push(std::mem::take(&mut cur));
                started = false;
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_quote = true;
            started = true;
        } else if c.is_whitespace() {
            if started {
                out.push(std::mem::take(&mut cur));
                started = false;
            }
        } else {
            cur.push(c);
            started = true;
        }
    }
    if started {
        out.push(cur);
    }
    out
}

// ---- instruction switches ----

/// Split a field's argument string into the part before the first `\switch` and
/// the `(switch-letter, argument)` pairs that follow. Field expressions don't
/// contain backslashes, so the first `\` reliably begins the switches.
fn split_switches(s: &str) -> (String, Vec<(char, String)>) {
    let cut = s.find('\\').unwrap_or(s.len());
    let head = s[..cut].trim().to_string();
    let mut switches = Vec::new();
    let chars: Vec<char> = s[cut..].chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            let letter = chars[i + 1];
            i += 2;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            // The argument is a quoted string or a bare token.
            let mut arg = String::new();
            if i < chars.len() && chars[i] == '"' {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    arg.push(chars[i]);
                    i += 1;
                }
                i += 1; // closing quote
            } else {
                while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '\\' {
                    arg.push(chars[i]);
                    i += 1;
                }
            }
            switches.push((letter, arg));
        } else {
            i += 1;
        }
    }
    (head, switches)
}

// ---- `= (Formula)` evaluation ----

/// Evaluate an arithmetic formula to a number. Supports `+ - * / % ^`, comparison
/// operators (yielding 1/0), parentheses, unary signs, and the spec's functions on
/// explicit numeric arguments. Returns `None` for anything we can't compute (e.g. a
/// bookmark or table-cell reference like `SUM(ABOVE)`), so the cached result stands.
fn eval_formula(expr: &str) -> Option<f64> {
    let mut p = Expr {
        chars: expr.chars().collect(),
        pos: 0,
    };
    let v = p.comparison()?;
    p.skip_ws();
    if p.pos == p.chars.len() {
        Some(v)
    } else {
        None
    }
}

struct Expr {
    chars: Vec<char>,
    pos: usize,
}

impl Expr {
    fn skip_ws(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }
    fn peek(&mut self) -> Option<char> {
        self.skip_ws();
        self.chars.get(self.pos).copied()
    }
    /// Consume `op` (after whitespace) if it's next.
    fn eat(&mut self, op: &str) -> bool {
        self.skip_ws();
        let op: Vec<char> = op.chars().collect();
        if self.chars[self.pos..].starts_with(&op) {
            self.pos += op.len();
            true
        } else {
            false
        }
    }

    fn comparison(&mut self) -> Option<f64> {
        let a = self.add()?;
        // Longer operators first so `<=`/`>=`/`<>` win over `<`/`>`.
        for (op, f) in [
            ("<=", 0u8),
            (">=", 1),
            ("<>", 2),
            ("=", 3),
            ("<", 4),
            (">", 5),
        ] {
            if self.eat(op) {
                let b = self.add()?;
                let r = match f {
                    0 => a <= b,
                    1 => a >= b,
                    2 => a != b,
                    3 => a == b,
                    4 => a < b,
                    _ => a > b,
                };
                return Some(if r { 1.0 } else { 0.0 });
            }
        }
        Some(a)
    }

    fn add(&mut self) -> Option<f64> {
        let mut a = self.mul()?;
        loop {
            if self.eat("+") {
                a += self.mul()?;
            } else if self.eat("-") {
                a -= self.mul()?;
            } else {
                return Some(a);
            }
        }
    }

    fn mul(&mut self) -> Option<f64> {
        let mut a = self.pow()?;
        loop {
            if self.eat("*") {
                a *= self.pow()?;
            } else if self.eat("/") {
                let b = self.pow()?;
                if b == 0.0 {
                    return None;
                }
                a /= b;
            } else if self.eat("%") {
                let b = self.pow()?;
                if b == 0.0 {
                    return None;
                }
                a %= b;
            } else {
                return Some(a);
            }
        }
    }

    fn pow(&mut self) -> Option<f64> {
        let a = self.unary()?;
        if self.eat("^") {
            let b = self.pow()?; // right-associative
            Some(a.powf(b))
        } else {
            Some(a)
        }
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat("-") {
            return Some(-self.unary()?);
        }
        if self.eat("+") {
            return self.unary();
        }
        self.primary()
    }

    fn primary(&mut self) -> Option<f64> {
        if self.eat("(") {
            let v = self.comparison()?;
            if !self.eat(")") {
                return None;
            }
            return Some(v);
        }
        match self.peek()? {
            c if c.is_ascii_digit() || c == '.' => self.number(),
            c if c.is_ascii_alphabetic() => self.func(),
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f64> {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.chars.len()
            && (self.chars[self.pos].is_ascii_digit() || self.chars[self.pos] == '.')
        {
            self.pos += 1;
        }
        let s: String = self.chars[start..self.pos].iter().collect();
        s.parse().ok()
    }

    fn func(&mut self) -> Option<f64> {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.chars.len() && self.chars[self.pos].is_ascii_alphabetic() {
            self.pos += 1;
        }
        let name: String = self.chars[start..self.pos].iter().collect();
        let name = name.to_ascii_uppercase();
        // Niladic constants.
        match name.as_str() {
            "TRUE" => return Some(1.0),
            "FALSE" => return Some(0.0),
            _ => {}
        }
        if !self.eat("(") {
            return None;
        }
        let mut args = Vec::new();
        if self.peek() != Some(')') {
            loop {
                args.push(self.comparison()?);
                if self.eat(",") {
                    continue;
                }
                break;
            }
        }
        if !self.eat(")") {
            return None;
        }
        apply_func(&name, &args)
    }
}

fn apply_func(name: &str, a: &[f64]) -> Option<f64> {
    let sum = || a.iter().sum::<f64>();
    Some(match name {
        "SUM" => sum(),
        "PRODUCT" => a.iter().product(),
        "COUNT" => a.len() as f64,
        "AVERAGE" => {
            if a.is_empty() {
                return None;
            }
            sum() / a.len() as f64
        }
        "MIN" => a.iter().copied().fold(f64::INFINITY, f64::min),
        "MAX" => a.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        "ABS" => a.first()?.abs(),
        "INT" => a.first()?.trunc(),
        "SIGN" => {
            let v = *a.first()?;
            if v > 0.0 {
                1.0
            } else if v < 0.0 {
                -1.0
            } else {
                0.0
            }
        }
        "MOD" => {
            let b = *a.get(1)?;
            if b == 0.0 {
                return None;
            }
            a.first()? % b
        }
        "ROUND" => {
            let n = *a.get(1)? as i32;
            let f = 10f64.powi(n);
            (a.first()? * f).round() / f
        }
        "AND" => (a.iter().all(|v| *v != 0.0)) as u8 as f64,
        "OR" => (a.iter().any(|v| *v != 0.0)) as u8 as f64,
        "NOT" => (*a.first()? == 0.0) as u8 as f64,
        _ => return None,
    })
}

// ---- result formatting ----

/// A bare number with trailing zeros trimmed (Word's default when no `\#`).
fn default_number(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Format a number against a `\#` picture (a simplified Excel number format):
/// `0`/`#` digits, `.` decimal, `,` grouping, `%` percent, a `positive;negative`
/// split, and surrounding literal text.
fn format_numeric(value: f64, pic: &str) -> String {
    let pic = pic.trim();
    let sections: Vec<&str> = pic.split(';').collect();
    let negative_section = value < 0.0 && sections.len() >= 2;
    let sec = if negative_section {
        sections[1]
    } else {
        sections[0]
    };
    let mut v = if negative_section { value.abs() } else { value };

    if sec.contains('%') {
        v *= 100.0;
    }
    let decimals = sec
        .split_once('.')
        .map(|(_, frac)| frac.chars().filter(|c| *c == '0' || *c == '#').count())
        .unwrap_or(0);
    let grouping = sec.contains(',');

    let rounded = format!("{:.*}", decimals, v.abs());
    let (int_part, frac_part) = match rounded.split_once('.') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (rounded, String::new()),
    };
    let mut num = if grouping {
        group_thousands(&int_part)
    } else {
        int_part
    };
    if decimals > 0 {
        num.push('.');
        num.push_str(&frac_part);
    }
    // A bare negative (no negative section) keeps a leading minus.
    if value < 0.0 && !negative_section {
        num.insert(0, '-');
    }

    // Splice the formatted number into the picture's literal prefix/suffix.
    let is_core = |c: char| "0#,.".contains(c);
    match (sec.find(is_core), sec.rfind(is_core)) {
        (Some(a), Some(b)) => {
            let lit = |s: &str| s.replace(['\'', '"'], "");
            format!("{}{num}{}", lit(&sec[..a]), lit(&sec[b + 1..]))
        }
        _ => num,
    }
}

fn group_thousands(int_part: &str) -> String {
    let digits: Vec<char> = int_part.chars().collect();
    let mut out = String::new();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

/// Apply a `\*` format switch: a number format (Arabic/roman/ordinal/cardinal/
/// alphabetic) on the integer value, or a text case transform on the string.
fn apply_star(text: &str, value: f64, fmt: &str) -> String {
    let n = value.round() as i64;
    match fmt.trim().to_ascii_lowercase().as_str() {
        "arabic" => n.to_string(),
        "roman" => roman(n, true),
        "roman_lower" | "lroman" => roman(n, false),
        "ordinal" => ordinal(n),
        "cardinal" => text.to_string(), // words are out of scope; keep the number
        "alphabetic" => alphabetic(n, true),
        "alphabetic_lower" | "alphabetic-lower" => alphabetic(n, false),
        "upper" | "uppercase" => text.to_uppercase(),
        "lower" | "lowercase" => text.to_lowercase(),
        "caps" => title_case(text),
        "firstcap" => first_cap(text),
        _ => text.to_string(), // MERGEFORMAT, CHARFORMAT, unknown: no change
    }
}

fn roman(mut n: i64, upper: bool) -> String {
    if !(1..=3999).contains(&n) {
        return n.to_string();
    }
    const VALS: [(i64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (v, sym) in VALS {
        while n >= v {
            out.push_str(sym);
            n -= v;
        }
    }
    if upper { out } else { out.to_lowercase() }
}

fn ordinal(n: i64) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// 1→A, 26→Z, 27→AA … (or lowercase).
fn alphabetic(n: i64, upper: bool) -> String {
    if n < 1 {
        return n.to_string();
    }
    let mut n = n;
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        let c = (b'A' + (n % 26) as u8) as char;
        out.push(c);
        n /= 26;
    }
    let s: String = out.iter().rev().collect();
    if upper { s } else { s.to_lowercase() }
}

fn title_case(s: &str) -> String {
    s.split(' ').map(first_cap).collect::<Vec<_>>().join(" ")
}

fn first_cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

// ---- context-dependent fields (date/time, document properties) ----

/// A civil date-time, the unit DATE/CREATEDATE/… fields format.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    /// 0 = Sunday … 6 = Saturday.
    pub weekday: u32,
}

/// Document properties from `docProps/core.xml`, the source for AUTHOR/TITLE/… and
/// CREATEDATE/SAVEDATE fields.
#[derive(Clone, Debug, Default)]
pub struct DocProps {
    pub author: String,
    pub title: String,
    pub subject: String,
    pub keywords: String,
    pub comments: String,
    pub last_saved_by: String,
    pub revision: String,
    pub created: Option<DateTime>,
    pub modified: Option<DateTime>,
}

/// What a field needs beyond its own instruction: the current clock (for DATE/
/// TIME), the document properties, and the file name.
#[derive(Clone, Debug, Default)]
pub struct FieldContext {
    pub now: Option<DateTime>,
    pub props: DocProps,
    pub filename: String,
}

/// Like [`eval_field`], but also resolves fields that need outside context — the
/// clock (DATE, TIME), the document properties (AUTHOR, TITLE, CREATEDATE, …), and
/// the file name (FILENAME). Returns `None` when the field still can't be resolved
/// (e.g. DATE with no clock, or an empty property), so the cached result stands.
pub fn eval_field_ctx(instr: &str, ctx: &FieldContext) -> Option<String> {
    if let Some(v) = eval_field(instr) {
        return Some(v); // self-contained: = (Formula), IF
    }
    let s = instr.trim();
    let name_end = s.find(char::is_whitespace).unwrap_or(s.len());
    let name = s[..name_end].to_ascii_uppercase();
    let rest = &s[name_end..];
    let (_, switches) = split_switches(rest);
    let pic = switches
        .iter()
        .find(|(c, _)| *c == '@')
        .map(|(_, a)| a.as_str());
    let nonempty = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());

    let mut out = match name.as_str() {
        // The current clock — already local — so DATE/TIME need no timezone math.
        "DATE" => format_date(ctx.now.as_ref()?, pic.unwrap_or("M/d/yyyy")),
        "TIME" => format_date(ctx.now.as_ref()?, pic.unwrap_or("h:mm AM/PM")),
        // CREATEDATE/SAVEDATE/PRINTDATE are deliberately NOT recomputed: core.xml
        // stores them in UTC, but Word shows them in the viewer's local time, so the
        // value Word already cached is the correct local representation. Recomputing
        // from UTC (we have no original timezone) would shift the time. Keep cached.
        "AUTHOR" => nonempty(&ctx.props.author)?,
        "TITLE" => nonempty(&ctx.props.title)?,
        "SUBJECT" => nonempty(&ctx.props.subject)?,
        "KEYWORDS" => nonempty(&ctx.props.keywords)?,
        "COMMENTS" => nonempty(&ctx.props.comments)?,
        "LASTSAVEDBY" => nonempty(&ctx.props.last_saved_by)?,
        "REVNUM" => nonempty(&ctx.props.revision)?,
        "FILENAME" => nonempty(&ctx.filename)?,
        _ => return None,
    };
    if let Some((_, fmt)) = switches.iter().find(|(c, _)| *c == '*') {
        out = apply_star(&out, out.parse().unwrap_or(0.0), fmt);
    }
    Some(out)
}

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
const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// Format a date-time against a Word `\@` picture (case-sensitive: `M`=month,
/// `m`=minute). Supports yyyy/yy, MMMM/MMM/MM/M, dddd/ddd/dd/d, HH/H, hh/h, mm/m,
/// ss/s, AM/PM (and am/pm), `'literal'` text, and other characters as literals.
fn format_date(dt: &DateTime, pic: &str) -> String {
    let chars: Vec<char> = pic.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let h12 = {
        let h = dt.hour % 12;
        if h == 0 { 12 } else { h }
    };
    let starts = |i: usize, pat: &str| chars[i..].starts_with(&pat.chars().collect::<Vec<_>>());
    while i < chars.len() {
        // Quoted literal.
        if chars[i] == '\'' {
            i += 1;
            while i < chars.len() && chars[i] != '\'' {
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        let (tok, val): (&str, String) = if starts(i, "yyyy") {
            ("yyyy", format!("{:04}", dt.year))
        } else if starts(i, "yy") {
            ("yy", format!("{:02}", dt.year.rem_euclid(100)))
        } else if starts(i, "MMMM") {
            (
                "MMMM",
                MONTHS
                    .get((dt.month.max(1) - 1) as usize)
                    .unwrap_or(&"")
                    .to_string(),
            )
        } else if starts(i, "MMM") {
            (
                "MMM",
                MONTHS
                    .get((dt.month.max(1) - 1) as usize)
                    .map(|m| m[..3].to_string())
                    .unwrap_or_default(),
            )
        } else if starts(i, "MM") {
            ("MM", format!("{:02}", dt.month))
        } else if starts(i, "M") {
            ("M", dt.month.to_string())
        } else if starts(i, "dddd") {
            (
                "dddd",
                WEEKDAYS.get(dt.weekday as usize).unwrap_or(&"").to_string(),
            )
        } else if starts(i, "ddd") {
            (
                "ddd",
                WEEKDAYS
                    .get(dt.weekday as usize)
                    .map(|w| w[..3].to_string())
                    .unwrap_or_default(),
            )
        } else if starts(i, "dd") {
            ("dd", format!("{:02}", dt.day))
        } else if starts(i, "d") {
            ("d", dt.day.to_string())
        } else if starts(i, "HH") {
            ("HH", format!("{:02}", dt.hour))
        } else if starts(i, "H") {
            ("H", dt.hour.to_string())
        } else if starts(i, "hh") {
            ("hh", format!("{h12:02}"))
        } else if starts(i, "h") {
            ("h", h12.to_string())
        } else if starts(i, "mm") {
            ("mm", format!("{:02}", dt.min))
        } else if starts(i, "m") {
            ("m", dt.min.to_string())
        } else if starts(i, "ss") {
            ("ss", format!("{:02}", dt.sec))
        } else if starts(i, "s") {
            ("s", dt.sec.to_string())
        } else if starts(i, "AM/PM") {
            ("AM/PM", if dt.hour < 12 { "AM" } else { "PM" }.to_string())
        } else if starts(i, "am/pm") {
            ("am/pm", if dt.hour < 12 { "am" } else { "pm" }.to_string())
        } else {
            out.push(chars[i]);
            i += 1;
            continue;
        };
        out.push_str(&val);
        i += tok.chars().count();
    }
    out
}

/// Day of week (0=Sunday) for a civil date, via days-from-civil.
fn weekday_of(y: i32, m: u32, d: u32) -> u32 {
    // days since 1970-01-01 (a Thursday).
    let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as i64;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era as i64 * 146097 + doe - 719468;
    (((days % 7) + 4 + 7) % 7) as u32 // 1970-01-01 = Thursday(4)
}

/// Convert Unix seconds (UTC) to a civil date-time (used as the non-Windows
/// fallback clock; Word shows local time, but this is close enough for DATE).
pub fn civil_from_unix(secs: i64) -> DateTime {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let weekday = (((days % 7) + 4 + 7) % 7) as u32; // 1970-01-01 = Thursday
    // days-from-civil inverse (Howard Hinnant).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (y + if month <= 2 { 1 } else { 0 }) as i32;
    DateTime {
        year,
        month,
        day,
        hour: (rem / 3600) as u32,
        min: (rem % 3600 / 60) as u32,
        sec: (rem % 60) as u32,
        weekday,
    }
}

/// Parse `docProps/core.xml` into [`DocProps`].
pub fn parse_core_props(xml: &str) -> DocProps {
    let tag = |name: &str| -> String {
        let open = format!("<{name}");
        let Some(s) = xml.find(&open) else {
            return String::new();
        };
        let Some(gt) = xml[s..].find('>').map(|e| s + e + 1) else {
            return String::new();
        };
        let close = format!("</{name}>");
        let Some(e) = xml[gt..].find(&close).map(|e| gt + e) else {
            return String::new();
        };
        xml_unescape(&xml[gt..e])
    };
    DocProps {
        author: tag("dc:creator"),
        title: tag("dc:title"),
        subject: tag("dc:subject"),
        keywords: tag("cp:keywords"),
        comments: tag("dc:description"),
        last_saved_by: tag("cp:lastModifiedBy"),
        revision: tag("cp:revision"),
        created: parse_iso(&tag("dcterms:created")),
        modified: parse_iso(&tag("dcterms:modified")),
    }
}

/// Parse an ISO-8601 timestamp like `2007-11-05T14:54:00Z` (the value is in UTC;
/// we format the stored components directly).
pub fn parse_iso(s: &str) -> Option<DateTime> {
    let s = s.trim();
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<u32>().ok();
    if s.len() < 10 {
        return None;
    }
    let year = s.get(0..4)?.parse::<i32>().ok()?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let (hour, min, sec) = if s.len() >= 19 {
        (num(11, 13)?, num(14, 16)?, num(17, 19)?)
    } else {
        (0, 0, 0)
    };
    Some(DateTime {
        year,
        month,
        day,
        hour,
        min,
        sec,
        weekday: weekday_of(year, month, day),
    })
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Extract the `w:instr` value (entity-decoded) from a `<w:fldSimple>`'s raw XML.
fn instr_of(raw: &str) -> Option<String> {
    let k = "w:instr=\"";
    let s = raw.find(k)? + k.len();
    let e = raw[s..].find('"')? + s;
    Some(xml_unescape(&raw[s..e]))
}

/// Recompute every simple field in the document against `ctx`, replacing its
/// rendered text where a fresh value is available (the cache stands otherwise).
pub fn recompute(doc: &mut crate::model::Document, ctx: &FieldContext) {
    fn walk_blocks(blocks: &mut [crate::model::Block], ctx: &FieldContext) {
        use crate::model::{Block, Inline};
        for b in blocks {
            match b {
                Block::Paragraph(p) => {
                    for inl in &mut p.content {
                        match inl {
                            Inline::Field { raw, text } => {
                                if let Some(v) = instr_of(raw).and_then(|i| eval_field_ctx(&i, ctx))
                                {
                                    *text = v;
                                }
                            }
                            Inline::TextBox { blocks, .. } => walk_blocks(blocks, ctx),
                            _ => {}
                        }
                    }
                }
                Block::Table(t) => {
                    for row in &mut t.rows {
                        for cell in &mut row.cells {
                            walk_blocks(&mut cell.blocks, ctx);
                        }
                    }
                }
                Block::Raw(_) => {}
            }
        }
    }
    walk_blocks(&mut doc.body, ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(instr: &str) -> Option<String> {
        eval_field(instr)
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(ev("= 2+2").as_deref(), Some("4"));
        assert_eq!(ev("= 2+3*4").as_deref(), Some("14"));
        assert_eq!(ev("= (2+3)*4").as_deref(), Some("20"));
        assert_eq!(ev("= 7/2").as_deref(), Some("3.5"));
        assert_eq!(ev("= 2^10").as_deref(), Some("1024"));
        assert_eq!(ev("= -3 + 5").as_deref(), Some("2"));
        assert_eq!(ev("= 10 % 3").as_deref(), Some("1"));
    }

    #[test]
    fn functions() {
        assert_eq!(ev("= SUM(1,2,3,4)").as_deref(), Some("10"));
        assert_eq!(ev("= AVERAGE(2,4,6)").as_deref(), Some("4"));
        assert_eq!(ev("= MAX(3,9,2)").as_deref(), Some("9"));
        assert_eq!(ev("= ROUND(3.14159, 2)").as_deref(), Some("3.14"));
        assert_eq!(ev("= ABS(-7)").as_deref(), Some("7"));
        assert_eq!(ev("= MOD(17,5)").as_deref(), Some("2"));
    }

    #[test]
    fn comparisons_yield_one_or_zero() {
        assert_eq!(ev("= 3 > 2").as_deref(), Some("1"));
        assert_eq!(ev("= 3 <= 2").as_deref(), Some("0"));
        assert_eq!(ev("= 5 <> 5").as_deref(), Some("0"));
    }

    #[test]
    fn numeric_picture() {
        assert_eq!(ev("= 1234.5 \\# \"0.00\"").as_deref(), Some("1234.50"));
        assert_eq!(ev("= 1234567 \\# \"#,##0\"").as_deref(), Some("1,234,567"));
        assert_eq!(ev("= 0.25 \\# \"0%\"").as_deref(), Some("25%"));
        assert_eq!(
            ev("= 1234.5 \\# \"$#,##0.00\"").as_deref(),
            Some("$1,234.50")
        );
    }

    #[test]
    fn star_format_switch() {
        assert_eq!(ev("= 4 \\* roman").as_deref(), Some("IV"));
        assert_eq!(ev("= 3 \\* ordinal").as_deref(), Some("3rd"));
        assert_eq!(ev("= 27 \\* alphabetic").as_deref(), Some("AA"));
    }

    #[test]
    fn if_field_picks_branch() {
        assert_eq!(ev("IF 3 > 2 \"yes\" \"no\"").as_deref(), Some("yes"));
        assert_eq!(ev("IF 1 >= 5 \"yes\" \"no\"").as_deref(), Some("no"));
        assert_eq!(
            ev("IF \"a\" = \"b\" \"same\" \"diff\"").as_deref(),
            Some("diff")
        );
        assert_eq!(
            ev("IF \"x\" <> \"y\" \"different\" \"equal\"").as_deref(),
            Some("different")
        );
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime {
        DateTime {
            year: y,
            month: mo,
            day: d,
            hour: h,
            min: mi,
            sec: s,
            weekday: weekday_of(y, mo, d),
        }
    }

    #[test]
    fn date_and_time_fields_use_the_clock() {
        let ctx = FieldContext {
            now: Some(at(2026, 6, 23, 14, 5, 9)),
            ..Default::default()
        };
        // simple-field.docx: DATE with only \* MERGEFORMAT → default M/d/yyyy
        assert_eq!(
            eval_field_ctx(" DATE \\* MERGEFORMAT ", &ctx).as_deref(),
            Some("6/23/2026")
        );
        assert_eq!(
            eval_field_ctx(" DATE \\@ \"yyyy-MM-dd\" ", &ctx).as_deref(),
            Some("2026-06-23")
        );
        assert_eq!(
            eval_field_ctx(" TIME \\@ \"h:mm AM/PM\" ", &ctx).as_deref(),
            Some("2:05 PM")
        );
        // no clock → the cached value stands
        assert_eq!(eval_field_ctx(" DATE ", &FieldContext::default()), None);
    }

    #[test]
    fn date_picture_names_and_weekday() {
        let dt = at(2026, 6, 23, 0, 0, 0);
        assert_eq!(WEEKDAYS[dt.weekday as usize], "Tuesday");
        assert_eq!(
            format_date(&dt, "dddd, MMMM d, yyyy"),
            "Tuesday, June 23, 2026"
        );
        assert_eq!(format_date(&dt, "ddd MMM dd"), "Tue Jun 23");
    }

    #[test]
    fn document_property_fields() {
        let ctx = FieldContext {
            props: DocProps {
                author: "Zhe".into(),
                title: "My Doc".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(eval_field_ctx(" AUTHOR ", &ctx).as_deref(), Some("Zhe"));
        assert_eq!(
            eval_field_ctx(" TITLE \\* Upper ", &ctx).as_deref(),
            Some("MY DOC")
        );
        // an empty property is not substituted (the cache stands)
        assert_eq!(eval_field_ctx(" SUBJECT ", &ctx), None);
    }

    #[test]
    fn core_props_parse_and_stored_dates_keep_cache() {
        let props = parse_core_props(
            "<x><dc:creator>Zhe</dc:creator>\
             <dcterms:created>2007-11-05T14:54:00Z</dcterms:created></x>",
        );
        assert_eq!(props.author, "Zhe");
        assert_eq!(props.created, Some(at(2007, 11, 5, 14, 54, 0)));
        // CREATEDATE is not recomputed (UTC→local would shift it); cache stands.
        let ctx = FieldContext {
            props,
            ..Default::default()
        };
        assert_eq!(eval_field_ctx(" CREATEDATE \\@ \"M/d/yyyy\" ", &ctx), None);
    }

    #[test]
    fn non_formula_and_unsupported_return_none() {
        assert_eq!(ev(" DATE \\@ \"M/d/yyyy\" "), None);
        assert_eq!(ev(" PAGE "), None);
        // a table/bookmark reference we can't resolve falls back to the cache
        assert_eq!(ev("= SUM(ABOVE)"), None);
    }
}
