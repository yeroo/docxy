//! AutoCorrect for sheet entry (#667, ENT-112..121): the replace list, the
//! capitalisation rules, the exceptions and the AutoFormat As You Type
//! hyperlink rule, as pure functions over an editor buffer.
//!
//! The host calls [`AutoCorrect::correct`] when a word-ending character is
//! typed and [`AutoCorrect::correct_at_commit`] when the entry is committed,
//! applies the returned [`Correction`], and keeps it so that Ctrl+Z right
//! after it can put the typed text back ([`Correction::undo`]). Nothing here
//! runs on a formula entry or an apostrophe entry.
//!
//! Office shares one list between its applications; there is no such list
//! off Windows, so the built-in list here is a documented subset of Office's
//! ([`BUILTIN`]) and each host persists the user's changes to it
//! ([`AutoCorrect::to_lines`]).

use std::collections::{BTreeMap, BTreeSet};

/// The dialog's switches (ENT-113, ENT-120, ENT-121). `Default` is Office's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcOptions {
    /// Show AutoCorrect Options buttons.
    pub show_buttons: bool,
    /// Correct TWo INitial CApitals.
    pub two_initial_caps: bool,
    /// Capitalize first letter of sentences.
    pub first_letter: bool,
    /// Capitalize names of days.
    pub names_of_days: bool,
    /// Correct accidental use of cAPS LOCK key.
    pub caps_lock: bool,
    /// Replace text as you type.
    pub replace_text: bool,
    /// AutoFormat As You Type › Internet and network paths with hyperlinks.
    pub hyperlinks: bool,
    /// AutoFormat As You Type › Include new rows and columns in table
    /// (stored; the table rules live with the tables).
    pub table_rows_cols: bool,
    /// AutoFormat As You Type › Fill formulas in tables to create calculated
    /// columns (stored; the table rules live with the tables).
    pub table_formulas: bool,
    /// Actions › Enable additional actions in the right-click menu (stored).
    pub additional_actions: bool,
    /// Math AutoCorrect › Use Math AutoCorrect rules outside of math regions.
    pub math_outside: bool,
    /// Math AutoCorrect › Replace text as you type.
    pub math_replace: bool,
}

impl Default for AcOptions {
    fn default() -> Self {
        AcOptions {
            show_buttons: true,
            two_initial_caps: true,
            first_letter: true,
            names_of_days: true,
            caps_lock: true,
            replace_text: true,
            hyperlinks: true,
            table_rows_cols: true,
            table_formulas: true,
            additional_actions: false,
            math_outside: false,
            math_replace: true,
        }
    }
}

/// Each switch's persisted key, its dialog label, and its slot.
type Slot = fn(&mut AcOptions) -> &mut bool;

/// The switches in the dialog's order: (key, label, slot).
pub const SWITCHES: [(&str, &str, Slot); 12] = [
    ("ac_show_buttons", "Show AutoCorrect Options buttons", |o| {
        &mut o.show_buttons
    }),
    ("ac_two_initial_caps", "Correct TWo INitial CApitals", |o| {
        &mut o.two_initial_caps
    }),
    (
        "ac_first_letter",
        "Capitalize first letter of sentences",
        |o| &mut o.first_letter,
    ),
    ("ac_names_of_days", "Capitalize names of days", |o| {
        &mut o.names_of_days
    }),
    (
        "ac_caps_lock",
        "Correct accidental use of cAPS LOCK key",
        |o| &mut o.caps_lock,
    ),
    ("ac_replace_text", "Replace text as you type", |o| {
        &mut o.replace_text
    }),
    (
        "ac_hyperlinks",
        "Internet and network paths with hyperlinks",
        |o| &mut o.hyperlinks,
    ),
    (
        "ac_table_rows_cols",
        "Include new rows and columns in table",
        |o| &mut o.table_rows_cols,
    ),
    (
        "ac_table_formulas",
        "Fill formulas in tables to create calculated columns",
        |o| &mut o.table_formulas,
    ),
    (
        "ac_additional_actions",
        "Enable additional actions in the right-click menu",
        |o| &mut o.additional_actions,
    ),
    (
        "ac_math_outside",
        "Use Math AutoCorrect rules outside of math regions",
        |o| &mut o.math_outside,
    ),
    ("ac_math_replace", "Replace text as you type (Math)", |o| {
        &mut o.math_replace
    }),
];

impl AcOptions {
    /// The switch a persisted key names.
    pub fn slot(&mut self, key: &str) -> Option<&mut bool> {
        SWITCHES
            .iter()
            .find(|(k, _, _)| *k == key)
            .map(|(_, _, slot)| slot(self))
    }

    /// The value of the switch a persisted key names.
    pub fn get(&self, key: &str) -> Option<bool> {
        let mut copy = *self;
        copy.slot(key).map(|b| *b)
    }
}

/// The built-in replace list: a subset of Office's, none of whose keys reads
/// as a number, date, formula or other non-text entry (Office's `==>` would
/// start a formula)
/// (`no_builtin_key_is_a_non_text_entry`), so a typed number is never
/// rewritten.
pub const BUILTIN: &[(&str, &str)] = &[
    ("(c)", "\u{a9}"),
    ("(e)", "\u{20ac}"),
    ("(r)", "\u{ae}"),
    ("(tm)", "\u{2122}"),
    ("...", "\u{2026}"),
    ("-->", "\u{2192}"),
    ("<--", "\u{2190}"),
    ("<==", "\u{21d0}"),
    ("<=>", "\u{21d4}"),
    ("abbout", "about"),
    ("abotu", "about"),
    ("accomodate", "accommodate"),
    ("acheive", "achieve"),
    ("acn", "can"),
    ("adn", "and"),
    ("agian", "again"),
    ("alot", "a lot"),
    ("anual", "annual"),
    ("aslo", "also"),
    ("becuase", "because"),
    ("beleive", "believe"),
    ("calender", "calendar"),
    ("collegue", "colleague"),
    ("definately", "definitely"),
    ("didnt", "didn't"),
    ("doesnt", "doesn't"),
    ("dont", "don't"),
    ("enviroment", "environment"),
    ("existance", "existence"),
    ("goverment", "government"),
    ("hte", "the"),
    ("isnt", "isn't"),
    ("occured", "occurred"),
    ("occurence", "occurrence"),
    ("recieve", "receive"),
    ("recieved", "received"),
    ("seperate", "separate"),
    ("shouldnt", "shouldn't"),
    ("teh", "the"),
    ("thier", "their"),
    ("tommorow", "tomorrow"),
    ("untill", "until"),
    ("wasnt", "wasn't"),
    ("wich", "which"),
    ("wierd", "weird"),
    ("wouldnt", "wouldn't"),
    ("yuor", "your"),
];

/// The built-in First Letter exceptions: abbreviations after which the next
/// word is not capitalised.
pub const BUILTIN_FIRST_LETTER: &[&str] = &[
    "abbr.", "approx.", "apr.", "aug.", "dec.", "dept.", "e.g.", "etc.", "feb.", "i.e.", "inc.",
    "jan.", "jul.", "jun.", "mar.", "no.", "nov.", "oct.", "sep.", "vs.",
];

/// Math AutoCorrect's built-in list (a subset), applied outside equations
/// only with [`AcOptions::math_outside`] on.
pub const MATH: &[(&str, &str)] = &[
    ("\\alpha", "\u{3b1}"),
    ("\\beta", "\u{3b2}"),
    ("\\deg", "\u{b0}"),
    ("\\delta", "\u{3b4}"),
    ("\\div", "\u{f7}"),
    ("\\ge", "\u{2265}"),
    ("\\infty", "\u{221e}"),
    ("\\le", "\u{2264}"),
    ("\\ne", "\u{2260}"),
    ("\\pi", "\u{3c0}"),
    ("\\pm", "\u{b1}"),
    ("\\rightarrow", "\u{2192}"),
    ("\\sigma", "\u{3c3}"),
    ("\\times", "\u{d7}"),
];

const DAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// One replacement in an editor buffer: the chars `start..start + from`
/// held `from` and now hold `to`. Offsets count chars, as the editors' carets
/// do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Correction {
    pub start: usize,
    pub from: String,
    pub to: String,
}

impl Correction {
    /// `buf` with the correction made (the chars at `start` are `from`).
    pub fn apply(&self, buf: &str) -> String {
        splice(buf, self.start, self.from.chars().count(), &self.to)
    }

    /// `buf` with the correction taken back, when the corrected text is still
    /// where it was put; `None` once it has been edited away.
    pub fn undo(&self, buf: &str) -> Option<String> {
        let n = self.to.chars().count();
        let here: String = buf.chars().skip(self.start).take(n).collect();
        (here == self.to).then(|| splice(buf, self.start, n, &self.from))
    }

    /// How far the caret behind the corrected word moves when the correction
    /// is made (negative when it shortens the buffer).
    pub fn shift(&self) -> isize {
        self.to.chars().count() as isize - self.from.chars().count() as isize
    }
}

fn splice(buf: &str, start: usize, len: usize, with: &str) -> String {
    let mut out: String = buf.chars().take(start).collect();
    out.push_str(with);
    out.extend(buf.chars().skip(start + len));
    out
}

/// Why the dialog's Add or an exception edit was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcError {
    /// Replace is empty, or holds a space, tab or line break.
    BadReplace,
    /// With holds a tab or a line break.
    BadWith,
}

impl std::fmt::Display for AcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AcError::BadReplace => "Replace must be one word with no spaces",
            AcError::BadWith => "With cannot hold a tab or a line break",
        })
    }
}

impl std::error::Error for AcError {}

/// Which exceptions list (ENT-117's two tabs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExceptionKind {
    /// First Letter: words after which a capital is not forced.
    FirstLetter,
    /// INitial CAps: words whose two initial capitals are kept.
    InitialCaps,
}

impl ExceptionKind {
    /// The name the control verbs use.
    pub fn name(self) -> &'static str {
        match self {
            ExceptionKind::FirstLetter => "first_letter",
            ExceptionKind::InitialCaps => "initial_caps",
        }
    }

    pub fn from_name(s: &str) -> Option<ExceptionKind> {
        match s {
            "first_letter" | "first-letter" | "First Letter" => Some(ExceptionKind::FirstLetter),
            "initial_caps" | "initial-caps" | "INitial CAps" => Some(ExceptionKind::InitialCaps),
            _ => None,
        }
    }
}

/// The user's AutoCorrect: switches, replace list and exceptions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoCorrect {
    pub opts: AcOptions,
    /// Replace → With, keyed by the lowercased Replace.
    entries: BTreeMap<String, (String, String)>,
    /// First Letter exceptions, lowercased.
    first_letter: BTreeSet<String>,
    /// INitial CAps exceptions, as typed (matched exactly).
    initial_caps: BTreeSet<String>,
}

impl Default for AutoCorrect {
    fn default() -> Self {
        AutoCorrect {
            opts: AcOptions::default(),
            entries: BUILTIN
                .iter()
                .map(|&(r, w)| (r.to_lowercase(), (r.to_string(), w.to_string())))
                .collect(),
            first_letter: BUILTIN_FIRST_LETTER.iter().map(|s| s.to_string()).collect(),
            initial_caps: BTreeSet::new(),
        }
    }
}

fn check_replace(replace: &str) -> Result<(), AcError> {
    if replace.is_empty() || replace.chars().any(char::is_whitespace) {
        return Err(AcError::BadReplace);
    }
    Ok(())
}

impl AutoCorrect {
    /// The replace list as (Replace, With), sorted by Replace ignoring case.
    pub fn entries(&self) -> Vec<(String, String)> {
        self.entries.values().cloned().collect()
    }

    /// The With an entry for `replace` holds, any case.
    pub fn lookup(&self, replace: &str) -> Option<&str> {
        self.entries
            .get(&replace.to_lowercase())
            .map(|(_, w)| w.as_str())
    }

    /// Add `replace` → `with`, replacing an entry for the same word (the
    /// host asks first, ENT-116); the With it replaced, if any.
    pub fn add(&mut self, replace: &str, with: &str) -> Result<Option<String>, AcError> {
        check_replace(replace)?;
        if with.contains(['\t', '\n', '\r']) {
            return Err(AcError::BadWith);
        }
        Ok(self
            .entries
            .insert(
                replace.to_lowercase(),
                (replace.to_string(), with.to_string()),
            )
            .map(|(_, w)| w))
    }

    /// Delete the entry for `replace`, any case. False when there was none.
    pub fn delete(&mut self, replace: &str) -> bool {
        self.entries.remove(&replace.to_lowercase()).is_some()
    }

    /// One exceptions list, sorted.
    pub fn exceptions(&self, kind: ExceptionKind) -> Vec<String> {
        match kind {
            ExceptionKind::FirstLetter => self.first_letter.iter().cloned().collect(),
            ExceptionKind::InitialCaps => self.initial_caps.iter().cloned().collect(),
        }
    }

    /// Add a word to an exceptions list. False when it was there already.
    pub fn add_exception(&mut self, kind: ExceptionKind, word: &str) -> Result<bool, AcError> {
        check_replace(word)?;
        Ok(match kind {
            ExceptionKind::FirstLetter => self.first_letter.insert(word.to_lowercase()),
            ExceptionKind::InitialCaps => self.initial_caps.insert(word.to_string()),
        })
    }

    /// Delete a word from an exceptions list. False when it was not there.
    pub fn delete_exception(&mut self, kind: ExceptionKind, word: &str) -> bool {
        match kind {
            ExceptionKind::FirstLetter => self.first_letter.remove(&word.to_lowercase()),
            ExceptionKind::InitialCaps => self.initial_caps.remove(word),
        }
    }

    /// The correction for the word that ends at char `end` of `buf`, the
    /// editor buffer: called when a space or punctuation is typed after a
    /// word (`end` is where that character went) and, through
    /// [`AutoCorrect::correct_at_commit`], when the entry is committed. The
    /// word is the run of non-space characters before `end`. `None` for a
    /// formula or apostrophe entry, or when no rule changes the word.
    ///
    /// The rules, in Office's order, each seeing the previous one's result:
    /// the replace list (and Math AutoCorrect outside math regions), TWo
    /// INitial CApitals, names of days, the first letter of a sentence (a
    /// word after `.`, `!` or `?` inside the entry — never the entry's
    /// first word, which Excel leaves alone), and accidental cAPS LOCK.
    pub fn correct(&self, buf: &str, end: usize) -> Option<Correction> {
        if is_formula_or_quoted(buf) {
            return None;
        }
        let chars: Vec<char> = buf.chars().collect();
        let end = end.min(chars.len());
        let mut start = end;
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        if start == end {
            return None;
        }
        let token: String = chars[start..end].iter().collect();
        let mut word = self.replaced(&token).unwrap_or_else(|| token.clone());
        // The capitalisation rules read the word without the punctuation
        // around it: `(monday)`, `THursday,`.
        let (lead, core, trail) = split_punct(&word);
        let mut core = core.to_string();
        if self.opts.two_initial_caps && !self.initial_caps.contains(&core) {
            if let Some(fixed) = two_initial_caps(&core) {
                core = fixed;
            }
        }
        if self.opts.names_of_days && DAYS.contains(&core.as_str()) {
            core = capitalize(&core);
        }
        if self.opts.first_letter && self.starts_sentence(&chars[..start]) {
            if let Some(c) = core.chars().next().filter(|c| c.is_lowercase()) {
                core = c.to_uppercase().chain(core.chars().skip(1)).collect();
            }
        }
        if self.opts.caps_lock {
            if let Some(fixed) = caps_lock(&core) {
                core = fixed;
            }
        }
        word = format!("{lead}{core}{trail}");
        (word != token).then_some(Correction {
            start,
            from: token,
            to: word,
        })
    }

    /// [`AutoCorrect::correct`] for the last word, at the commit of the whole
    /// buffer — except an entry that reads as a number, date, logical or
    /// error (`1/2` stays a date) is left alone.
    pub fn correct_at_commit(&self, buf: &str) -> Option<Correction> {
        if !reads_as_text(buf) {
            return None;
        }
        self.correct(buf, buf.chars().count())
    }

    /// The replace list's (then Math AutoCorrect's) With for a typed token:
    /// the whole token first, else the token without its leading and
    /// trailing punctuation (`teh,` is `teh`). An all-lowercase Replace
    /// matches any case and passes the typed word's capitals on (`Teh` →
    /// `The`, `TEH` → `THE`); one with a capital matches exactly.
    fn replaced(&self, token: &str) -> Option<String> {
        let whole = self.replace_one(token);
        if whole.is_some() {
            return whole;
        }
        let (lead, core, trail) = split_punct(token);
        if core.is_empty() || core.len() == token.len() {
            return None;
        }
        self.replace_one(core)
            .map(|with| format!("{lead}{with}{trail}"))
    }

    fn replace_one(&self, word: &str) -> Option<String> {
        if self.opts.replace_text {
            if let Some((replace, with)) = self.entries.get(&word.to_lowercase()) {
                let exact = replace.chars().any(char::is_uppercase);
                if !exact {
                    return Some(match_case(word, with));
                }
                if replace == word {
                    return Some(with.clone());
                }
            }
        }
        if self.opts.math_outside && self.opts.math_replace {
            if let Some((_, with)) = MATH.iter().find(|(r, _)| *r == word) {
                return Some(with.to_string());
            }
        }
        None
    }

    /// Does the word after `before` (the buffer up to the word) start a
    /// sentence inside the entry: the previous word ends in `.`, `!` or `?`
    /// and is not a First Letter exception?
    fn starts_sentence(&self, before: &[char]) -> bool {
        let mut end = before.len();
        if end == 0 || !before[end - 1].is_whitespace() {
            return false;
        }
        while end > 0 && before[end - 1].is_whitespace() {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && !before[start - 1].is_whitespace() {
            start -= 1;
        }
        let prev: String = before[start..end].iter().collect();
        let Some(last) = prev.chars().last() else {
            return false;
        };
        matches!(last, '.' | '!' | '?') && !self.first_letter.contains(&prev.to_lowercase())
    }

    /// The hyperlink a committed entry becomes with Internet and network
    /// paths with hyperlinks on (ENT-120): the whole entry one URL, `www.`
    /// address, e-mail address or UNC path. The target is what the link
    /// opens: `www.x.com` opens `http://www.x.com`, `a@b.com`
    /// `mailto:a@b.com`.
    pub fn hyperlink(&self, entry: &str) -> Option<String> {
        if !self.opts.hyperlinks {
            return None;
        }
        hyperlink_target(entry)
    }

    /// The `key=value` lines a host persists: the switches, then only the
    /// changes from the built-in lists (`ac_add=Replace\tWith`,
    /// `ac_del=Replace`, `ac_first_add=`/`ac_first_del=`,
    /// `ac_caps_add=`/`ac_caps_del=`), so a missing file is the defaults
    /// and a later built-in entry reaches every user who did not delete it.
    pub fn to_lines(&self) -> String {
        let mut out = String::new();
        for (key, _, _) in SWITCHES {
            let on = self.opts.get(key).unwrap_or(false);
            out.push_str(&format!("{key}={}\n", u8::from(on)));
        }
        let base = AutoCorrect::default();
        for (k, (r, w)) in &self.entries {
            if base.entries.get(k) != Some(&(r.clone(), w.clone())) {
                out.push_str(&format!("ac_add={r}\t{w}\n"));
            }
        }
        for (k, (r, _)) in &base.entries {
            if !self.entries.contains_key(k) {
                out.push_str(&format!("ac_del={r}\n"));
            }
        }
        let sets = [
            ("ac_first", &self.first_letter, &base.first_letter),
            ("ac_caps", &self.initial_caps, &base.initial_caps),
        ];
        for (key, mine, builtin) in sets {
            for w in mine.difference(builtin) {
                out.push_str(&format!("{key}_add={w}\n"));
            }
            for w in builtin.difference(mine) {
                out.push_str(&format!("{key}_del={w}\n"));
            }
        }
        out
    }

    /// What [`AutoCorrect::to_lines`] wrote, over the defaults; other keys
    /// and malformed lines are ignored.
    pub fn from_text(text: &str) -> AutoCorrect {
        let mut ac = AutoCorrect::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let k = k.trim();
            match k {
                "ac_add" => {
                    if let Some((r, w)) = v.split_once('\t') {
                        let _ = ac.add(r, w);
                    }
                }
                "ac_del" => {
                    ac.delete(v);
                }
                "ac_first_add" => {
                    let _ = ac.add_exception(ExceptionKind::FirstLetter, v);
                }
                "ac_first_del" => {
                    ac.delete_exception(ExceptionKind::FirstLetter, v);
                }
                "ac_caps_add" => {
                    let _ = ac.add_exception(ExceptionKind::InitialCaps, v);
                }
                "ac_caps_del" => {
                    ac.delete_exception(ExceptionKind::InitialCaps, v);
                }
                _ => {
                    let flag = match v.trim() {
                        "1" => Some(true),
                        "0" => Some(false),
                        t if t.eq_ignore_ascii_case("true") => Some(true),
                        t if t.eq_ignore_ascii_case("false") => Some(false),
                        _ => None,
                    };
                    if let (Some(slot), Some(b)) = (ac.opts.slot(k), flag) {
                        *slot = b;
                    }
                }
            }
        }
        ac
    }
}

/// A buffer AutoCorrect never touches: a formula (`=`, or the `+`, `-`, `@`
/// that start one while typing, ENT-118) or an apostrophe entry, which is
/// typed exactly.
fn is_formula_or_quoted(buf: &str) -> bool {
    matches!(buf.chars().next(), Some('=' | '+' | '-' | '@' | '\''))
}

/// Does `buf` commit as text in a General cell?
fn reads_as_text(buf: &str) -> bool {
    use crate::entry::{EntryCtx, parse_entry};
    use crate::sheet::{CellValue, Xf};
    let ctx = EntryCtx {
        today: Some(45_000.0),
        ..EntryCtx::default()
    };
    parse_entry(buf, &Xf::default(), &ctx)
        .is_ok_and(|e| e.cell.formula.is_none() && matches!(e.cell.value, CellValue::Text(_)))
}

/// `word` split into its leading punctuation, its core and its trailing
/// punctuation. The core starts and ends with a letter or digit.
fn split_punct(word: &str) -> (&str, &str, &str) {
    let alnum = |c: char| c.is_alphanumeric();
    let Some(first) = word.find(alnum) else {
        return (word, "", "");
    };
    let last = word
        .char_indices()
        .rev()
        .find(|&(_, c)| alnum(c))
        .map_or(word.len(), |(i, c)| i + c.len_utf8());
    (&word[..first], &word[first..last], &word[last..])
}

/// `with` in the case `typed` shows: all capitals for an all-capitals word
/// of two or more letters, an initial capital for a capitalised one.
fn match_case(typed: &str, with: &str) -> String {
    let letters: Vec<char> = typed.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase()) {
        return with.to_uppercase();
    }
    if letters.first().is_some_and(|c| c.is_uppercase()) {
        return capitalize(with);
    }
    with.to_string()
}

fn capitalize(word: &str) -> String {
    let mut cs = word.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().chain(cs).collect(),
        None => String::new(),
    }
}

/// `THursday` → `Thursday`: three or more letters, the first two capitals
/// and every other letter lowercase. A plural of capitals (`CDs`) is kept.
fn two_initial_caps(word: &str) -> Option<String> {
    let cs: Vec<char> = word.chars().collect();
    if cs.len() < 3 || !cs.iter().all(|c| c.is_alphabetic()) {
        return None;
    }
    let rest = &cs[2..];
    if !(cs[0].is_uppercase() && cs[1].is_uppercase()) || !rest.iter().all(|c| c.is_lowercase()) {
        return None;
    }
    if rest == ['s'] {
        return None;
    }
    let mut out = String::new();
    out.push(cs[0]);
    out.extend(cs[1].to_lowercase());
    out.extend(rest);
    Some(out)
}

/// `tHURSDAY` → `Thursday`: a lowercase first letter and every other letter
/// a capital, three or more letters.
fn caps_lock(word: &str) -> Option<String> {
    let cs: Vec<char> = word.chars().collect();
    if cs.len() < 3 || !cs.iter().all(|c| c.is_alphabetic()) {
        return None;
    }
    if !cs[0].is_lowercase() || !cs[1..].iter().all(|c| c.is_uppercase()) {
        return None;
    }
    let mut out: String = cs[0].to_uppercase().collect();
    for c in &cs[1..] {
        out.extend(c.to_lowercase());
    }
    Some(out)
}

/// What a typed URL, `www.` address, e-mail address or UNC path links to.
pub fn hyperlink_target(entry: &str) -> Option<String> {
    let t = entry.trim();
    if t.is_empty() || t.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let has_rest = |p: &str| lower.len() > p.len();
    for p in [
        "http://", "https://", "ftp://", "file://", "mailto:", "news:",
    ] {
        if lower.starts_with(p) {
            return has_rest(p).then(|| t.to_string());
        }
    }
    if lower.starts_with("www.") && has_rest("www.") && lower[4..].contains('.') {
        return Some(format!("http://{t}"));
    }
    if let Some(rest) = t.strip_prefix("\\\\") {
        let mut parts = rest.split('\\');
        let server = parts.next().unwrap_or("");
        let share = parts.next().unwrap_or("");
        return (!server.is_empty() && !share.is_empty()).then(|| t.to_string());
    }
    if let Some((user, host)) = t.split_once('@') {
        let ok = !user.is_empty()
            && !host.contains('@')
            && host.contains('.')
            && !host.starts_with('.')
            && !host.ends_with('.');
        return ok.then(|| format!("mailto:{t}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type `text` into an empty editor the way a host does: after each
    /// word-ending character the word before it is corrected, and at the end
    /// the commit corrects the last word.
    fn type_and_commit(ac: &AutoCorrect, text: &str) -> String {
        let mut buf = String::new();
        for ch in text.chars() {
            buf.push(ch);
            if ends_word(ch) {
                let at = buf.chars().count() - 1;
                if let Some(c) = ac.correct(&buf, at) {
                    buf = c.apply(&buf);
                }
            }
        }
        match ac.correct_at_commit(&buf) {
            Some(c) => c.apply(&buf),
            None => buf,
        }
    }

    fn ends_word(ch: char) -> bool {
        ch.is_whitespace() || matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | '"')
    }

    #[test]
    fn ent_case_040_corrects_text_and_leaves_formulas() {
        let ac = AutoCorrect::default();
        assert_eq!(type_and_commit(&ac, "(c) 2024"), "\u{a9} 2024");
        assert_eq!(type_and_commit(&ac, "teh cat"), "the cat");
        assert_eq!(type_and_commit(&ac, "=\"(c)\""), "=\"(c)\"");
        assert_eq!(type_and_commit(&ac, "monday meeting"), "Monday meeting");
        assert_eq!(type_and_commit(&ac, "THursday"), "Thursday");
        assert_eq!(type_and_commit(&ac, "teh"), "the", "corrected at commit");
    }

    #[test]
    fn ent_case_040_ctrl_z_takes_back_only_the_correction() {
        // A7: `teh ` corrects on the space; Ctrl+Z puts `teh ` back; `x`.
        let ac = AutoCorrect::default();
        let typed = "teh ";
        let c = ac.correct(typed, 3).expect("the space corrects teh");
        let corrected = c.apply(typed);
        assert_eq!(corrected, "the ");
        let back = c.undo(&corrected).expect("still there");
        assert_eq!(back, "teh ");
        let buf = format!("{back}x");
        let done = ac
            .correct_at_commit(&buf)
            .map_or(buf.clone(), |c| c.apply(&buf));
        assert_eq!(done, "teh x");
        // Once the corrected text is edited away there is nothing to undo.
        assert_eq!(c.undo("thx "), None);
    }

    #[test]
    fn the_replace_list_matches_case_and_punctuation() {
        let ac = AutoCorrect::default();
        assert_eq!(type_and_commit(&ac, "Teh cat"), "The cat");
        assert_eq!(type_and_commit(&ac, "TEH CAT"), "THE CAT");
        assert_eq!(type_and_commit(&ac, "teh, adn"), "the, and");
        assert_eq!(type_and_commit(&ac, "x (tm) y"), "x \u{2122} y");
        assert_eq!(type_and_commit(&ac, "so ... on"), "so \u{2026} on");
        // A word inside a longer token (a URL, a path) is left alone.
        assert_eq!(type_and_commit(&ac, "www.teh.com x"), "www.teh.com x");
        // Off, nothing is replaced.
        let mut off = AutoCorrect::default();
        off.opts.replace_text = false;
        assert_eq!(type_and_commit(&off, "teh cat"), "teh cat");
    }

    #[test]
    fn capitalisation_rules() {
        let ac = AutoCorrect::default();
        // The entry's first word is never capitalised; a sentence's is.
        assert_eq!(type_and_commit(&ac, "done. next one"), "done. Next one");
        assert_eq!(type_and_commit(&ac, "really? yes"), "really? Yes");
        assert_eq!(type_and_commit(&ac, "see e.g. this"), "see e.g. this");
        // Plural capitals and all-capitals words stay.
        assert_eq!(type_and_commit(&ac, "CDs USA ID"), "CDs USA ID");
        assert_eq!(type_and_commit(&ac, "tHURSDAY"), "Thursday");
        assert_eq!(type_and_commit(&ac, "(friday)"), "(Friday)");
        let mut off = AutoCorrect::default();
        off.opts.names_of_days = false;
        off.opts.two_initial_caps = false;
        off.opts.caps_lock = false;
        off.opts.first_letter = false;
        assert_eq!(
            type_and_commit(&off, "monday THursday tHURSDAY. x"),
            "monday THursday tHURSDAY. x"
        );
    }

    #[test]
    fn ent_case_056_entries_and_exceptions() {
        let mut ac = AutoCorrect::default();
        assert_eq!(
            ac.add("cdp", "Consolidated Data Processing"),
            Ok(None),
            "a new word"
        );
        assert_eq!(
            type_and_commit(&ac, "cdp report"),
            "Consolidated Data Processing report"
        );
        // Re-adding replaces and reports what it replaced (the host asks).
        assert_eq!(ac.lookup("CDP"), Some("Consolidated Data Processing"));
        assert_eq!(
            ac.add("cdp", "Other"),
            Ok(Some("Consolidated Data Processing".into()))
        );
        assert!(ac.delete("cdp"));
        assert!(!ac.delete("cdp"));
        assert_eq!(type_and_commit(&ac, "cdp report"), "cdp report");
        // INitial CAps exception.
        assert_eq!(type_and_commit(&ac, "ABc"), "Abc");
        assert_eq!(
            ac.add_exception(ExceptionKind::InitialCaps, "ABc"),
            Ok(true)
        );
        assert_eq!(type_and_commit(&ac, "ABc"), "ABc");
        // First Letter exception.
        assert_eq!(type_and_commit(&ac, "approx. ten"), "approx. ten");
        assert_eq!(type_and_commit(&ac, "fig. two"), "fig. Two");
        ac.add_exception(ExceptionKind::FirstLetter, "Fig.")
            .unwrap();
        assert_eq!(type_and_commit(&ac, "fig. two"), "fig. two");
        assert_eq!(ac.add("two words", "x"), Err(AcError::BadReplace));
        assert_eq!(ac.add("", "x"), Err(AcError::BadReplace));
        assert_eq!(ac.add("x", "a\tb"), Err(AcError::BadWith));
    }

    #[test]
    fn a_capitalised_replace_matches_exactly() {
        let mut ac = AutoCorrect::default();
        ac.add("MSft", "Microsoft").unwrap();
        assert_eq!(type_and_commit(&ac, "MSft x"), "Microsoft x");
        assert_eq!(type_and_commit(&ac, "msft x"), "msft x");
    }

    #[test]
    fn numbers_dates_formulas_and_quoted_entries_are_left_alone() {
        let ac = AutoCorrect::default();
        for t in ["1/2", "3.5", "(100)", "TRUE", "1e5"] {
            assert_eq!(type_and_commit(&ac, t), t);
        }
        for t in ["=teh", "+teh", "-teh", "@teh", "'teh cat"] {
            assert_eq!(type_and_commit(&ac, t), t);
        }
    }

    #[test]
    fn no_builtin_key_is_a_non_text_entry() {
        for (k, _) in BUILTIN {
            assert!(reads_as_text(k), "{k}");
            assert!(!k.chars().any(char::is_whitespace), "{k}");
            assert_eq!(*k, k.to_lowercase(), "{k} matches any case");
        }
        let mut keys: Vec<_> = BUILTIN.iter().map(|(k, _)| *k).collect();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n);
    }

    #[test]
    fn math_autocorrect_only_outside_math_regions_when_on() {
        let mut ac = AutoCorrect::default();
        assert_eq!(type_and_commit(&ac, "\\alpha x"), "\\alpha x");
        ac.opts.math_outside = true;
        assert_eq!(type_and_commit(&ac, "\\alpha x"), "\u{3b1} x");
        ac.opts.math_replace = false;
        assert_eq!(type_and_commit(&ac, "\\alpha x"), "\\alpha x");
    }

    #[test]
    fn hyperlinks() {
        let mut ac = AutoCorrect::default();
        assert_eq!(
            ac.hyperlink("https://example.com").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            ac.hyperlink("www.example.com").as_deref(),
            Some("http://www.example.com")
        );
        assert_eq!(
            ac.hyperlink("ada@example.com").as_deref(),
            Some("mailto:ada@example.com")
        );
        assert_eq!(
            ac.hyperlink("\\\\server\\share\\f.xlsx").as_deref(),
            Some("\\\\server\\share\\f.xlsx")
        );
        for t in [
            "see https://x.com",
            "https://",
            "www.",
            "a@b",
            "\\\\server",
            "plain",
        ] {
            assert_eq!(ac.hyperlink(t), None, "{t}");
        }
        ac.opts.hyperlinks = false;
        assert_eq!(ac.hyperlink("https://example.org"), None);
    }

    #[test]
    fn persistence_keeps_only_the_changes() {
        let d = AutoCorrect::default();
        assert_eq!(AutoCorrect::from_text(""), d);
        assert_eq!(AutoCorrect::from_text(&d.to_lines()), d);
        assert!(!d.to_lines().contains("ac_add="), "{}", d.to_lines());
        let mut ac = AutoCorrect::default();
        ac.add("cdp", "Consolidated Data Processing").unwrap();
        ac.add("teh", "THE").unwrap();
        ac.delete("adn");
        ac.add_exception(ExceptionKind::InitialCaps, "ABc").unwrap();
        ac.delete_exception(ExceptionKind::FirstLetter, "e.g.");
        ac.opts.hyperlinks = false;
        ac.opts.math_outside = true;
        let text = ac.to_lines();
        assert!(text.contains("ac_del=adn\n"), "{text}");
        assert!(text.contains("ac_add=teh\tTHE\n"), "{text}");
        assert_eq!(AutoCorrect::from_text(&text), ac);
        // Other preferences and junk lines are ignored.
        let mixed = format!("formula_view=1\nac_hyperlinks=maybe\nnonsense\n{text}");
        assert_eq!(AutoCorrect::from_text(&mixed), ac);
    }

    #[test]
    fn switches_are_named_once() {
        let mut o = AcOptions::default();
        for (i, (k, _, _)) in SWITCHES.iter().enumerate() {
            assert!(!SWITCHES[..i].iter().any(|(j, _, _)| j == k), "{k}");
            let before = o.get(k).unwrap();
            *o.slot(k).unwrap() = !before;
            assert_eq!(o.get(k), Some(!before));
        }
        assert_eq!(o.get("nope"), None);
    }
}
