//! AutoCorrect and AutoFormat As You Type for the Word editors (#856), as
//! pure functions over a paragraph's editor text.
//!
//! A host types a key through [`crate::editor::Editor::type_autocorrected`],
//! which asks [`fixes_for`] and [`auto_list`] what the key triggers and
//! applies each answer as an undo step of its own, named [`AUTOCORRECT`] or
//! [`AUTOFORMAT`] as in Word, so the first Ctrl+Z takes back the correction
//! and keeps the typing.
//!
//! The replace list and the First Letter exceptions come from the host
//! through [`WordFixes`]: the suite shares its one Office list
//! (`gridcore::autocorrect`) between its Sheet and Word tabs, and the terminal
//! uses [`BuiltinFixes`], a copy of that list's built-in entries (docxcore
//! depends on no sibling crate).

/// The undo step name of a replacement or a sentence capital.
pub const AUTOCORRECT: &str = "AutoCorrect";
/// The undo step name of a smart quote, an em dash or an automatic list.
pub const AUTOFORMAT: &str = "AutoFormat";

/// The switches. `Default` is Word's: everything on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoCorrectOptions {
    /// AutoCorrect › Replace text as you type.
    pub replace_text: bool,
    /// AutoCorrect › Capitalize first letter of sentences.
    pub capitalize_sentences: bool,
    /// AutoFormat As You Type › "Straight quotes" with “smart quotes”.
    pub smart_quotes: bool,
    /// AutoFormat As You Type › Hyphens (--) with dash (—).
    pub dashes: bool,
    /// AutoFormat As You Type › Automatic bulleted and numbered lists.
    pub auto_lists: bool,
}

impl Default for AutoCorrectOptions {
    fn default() -> Self {
        AutoCorrectOptions {
            replace_text: true,
            capitalize_sentences: true,
            smart_quotes: true,
            dashes: true,
            auto_lists: true,
        }
    }
}

/// The word rules a host's AutoCorrect list supplies.
pub trait WordFixes {
    /// The With for a typed token, in the case the token was typed in
    /// (`Teh` → `The`), or `None` when the list has no such Replace.
    fn replacement(&self, token: &str) -> Option<String>;
    /// Whether `word` (with its period, `e.g.`) is a First Letter exception:
    /// the word after it does not start a sentence.
    fn first_letter_exception(&self, word: &str) -> bool;
}

/// The built-in replace list: the same subset of Office's as
/// `gridcore::autocorrect::BUILTIN` (keep the two in step).
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

/// The built-in First Letter exceptions, as `gridcore`'s.
pub const BUILTIN_FIRST_LETTER: &[&str] = &[
    "abbr.", "approx.", "apr.", "aug.", "dec.", "dept.", "e.g.", "etc.", "feb.", "i.e.", "inc.",
    "jan.", "jul.", "jun.", "mar.", "no.", "nov.", "oct.", "sep.", "vs.",
];

/// [`BUILTIN`] and [`BUILTIN_FIRST_LETTER`], for a host with no list of its
/// own.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinFixes;

impl WordFixes for BuiltinFixes {
    fn replacement(&self, token: &str) -> Option<String> {
        let lower = token.to_lowercase();
        BUILTIN
            .iter()
            .find(|(replace, _)| *replace == lower)
            .map(|(_, with)| match_case(token, with))
    }

    fn first_letter_exception(&self, word: &str) -> bool {
        BUILTIN_FIRST_LETTER.contains(&word.to_lowercase().as_str())
    }
}

/// One correction: the chars `start..end` of the paragraph become `with`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    /// [`AUTOCORRECT`] or [`AUTOFORMAT`].
    pub name: &'static str,
    pub start: usize,
    pub end: usize,
    pub with: String,
}

impl Fix {
    fn apply(&self, text: &mut Vec<char>) {
        text.splice(self.start..self.end, self.with.chars());
    }
}

/// The list an automatic list puts its paragraph in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKind {
    Bullet,
    Decimal,
}

/// Does typing `ch` end the word before it? Whitespace, or punctuation that
/// closes a word.
pub fn ends_word(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
}

/// The corrections typing `ch` makes, in Word's order. `text` is the
/// paragraph's editor text with `ch` already typed just before `caret`.
/// Each fix's offsets are in the text the fixes before it left, so a host
/// applies them in order, each as its own undo step.
pub fn fixes_for(
    text: &[char],
    caret: usize,
    ch: char,
    opts: &AutoCorrectOptions,
    list: &dyn WordFixes,
) -> Vec<Fix> {
    if caret == 0 || text.get(caret - 1) != Some(&ch) {
        return Vec::new();
    }
    if matches!(ch, '"' | '\'') {
        return smart_quote(text, caret, ch, opts).into_iter().collect();
    }
    if !ends_word(ch) {
        return Vec::new();
    }
    let mut text = text.to_vec();
    let mut out = Vec::new();
    let mut push = |fix: Fix, text: &mut Vec<char>| {
        fix.apply(text);
        out.push(fix);
    };
    // A symbol Replace the key completes (`(c)` on its `)`, `...` on its
    // third `.`): it is the whole correction.
    if !ch.is_whitespace() {
        if let Some(fix) = symbol_fix(&text, caret, opts, list) {
            push(fix, &mut text);
            return out;
        }
    }
    // The word before the ender: it starts at `start` and ends at `end`
    // until a fix moves its end (`alot` → `a lot` is still one word for the
    // capital after it).
    let mut end = caret - 1;
    let start = token_start(&text, end);
    if let Some(fix) = dash_fix(&text, end, opts) {
        end = end + fix.with.chars().count() - (fix.end - fix.start);
        push(fix, &mut text);
    }
    if let Some(fix) = replace_fix(&text, end, opts, list) {
        end = end + fix.with.chars().count() - (fix.end - fix.start);
        push(fix, &mut text);
    }
    if ch.is_whitespace() {
        if let Some(fix) = capital_fix(&text, start, end, opts, list) {
            push(fix, &mut text);
        }
    }
    out
}

/// The list typing `ch` starts: a space after `1.`, `1)`, `*` or `-` that make
/// the whole paragraph. `in_list` is whether the paragraph is in a list already.
pub fn auto_list(
    text: &[char],
    caret: usize,
    ch: char,
    in_list: bool,
    opts: &AutoCorrectOptions,
) -> Option<ListKind> {
    if !opts.auto_lists || in_list || ch != ' ' || caret != text.len() {
        return None;
    }
    match text {
        ['1', '.' | ')', ' '] => Some(ListKind::Decimal),
        ['*' | '-', ' '] => Some(ListKind::Bullet),
        _ => None,
    }
}

/// A With as it can go into the text: a field's stand-in
/// ([`crate::editor::FIELD_CHAR`]) is never inserted, so it is left out here
/// too, and the fixes after it count the chars that really go in.
fn insertable(with: String) -> String {
    with.replace(crate::editor::FIELD_CHAR, "")
}

/// The start of the whitespace-delimited token that ends at `end`.
fn token_start(text: &[char], end: usize) -> usize {
    let mut start = end;
    while start > 0 && !text[start - 1].is_whitespace() {
        start -= 1;
    }
    start
}

fn symbol_fix(
    text: &[char],
    caret: usize,
    opts: &AutoCorrectOptions,
    list: &dyn WordFixes,
) -> Option<Fix> {
    if !opts.replace_text {
        return None;
    }
    let start = token_start(text, caret);
    // The whole token, then each tail that starts at punctuation, longest
    // first: `hello...` holds `...`.
    (start..caret)
        .filter(|&s| s == start || !text[s].is_alphanumeric())
        .find_map(|s| {
            let token: String = text[s..caret].iter().collect();
            if token.chars().all(char::is_alphanumeric) {
                return None;
            }
            list.replacement(&token).map(|with| Fix {
                name: AUTOCORRECT,
                start: s,
                end: caret,
                with: insertable(with),
            })
        })
}

/// `One--two` → `One—two`: two hyphens between letters or digits.
fn dash_fix(text: &[char], end: usize, opts: &AutoCorrectOptions) -> Option<Fix> {
    if !opts.dashes {
        return None;
    }
    let start = token_start(text, end);
    (start + 1..end.saturating_sub(2))
        .find(|&i| {
            text[i] == '-'
                && text[i + 1] == '-'
                && text[i - 1].is_alphanumeric()
                && text[i + 2].is_alphanumeric()
        })
        .map(|i| Fix {
            name: AUTOFORMAT,
            start: i,
            end: i + 2,
            with: "\u{2014}".into(),
        })
}

/// The replace list on the token before the ender: the whole token, else its
/// core without the punctuation around it (`(teh` is `teh`).
fn replace_fix(
    text: &[char],
    end: usize,
    opts: &AutoCorrectOptions,
    list: &dyn WordFixes,
) -> Option<Fix> {
    if !opts.replace_text {
        return None;
    }
    let start = token_start(text, end);
    if start == end {
        return None;
    }
    let token: String = text[start..end].iter().collect();
    if let Some(with) = list
        .replacement(&token)
        .map(insertable)
        .filter(|w| *w != token)
    {
        return Some(Fix {
            name: AUTOCORRECT,
            start,
            end,
            with,
        });
    }
    let (a, b) = core(text, start, end)?;
    if (a, b) == (start, end) {
        return None;
    }
    let word: String = text[a..b].iter().collect();
    list.replacement(&word)
        .map(insertable)
        .filter(|w| *w != word)
        .map(|with| Fix {
            name: AUTOCORRECT,
            start: a,
            end: b,
            with,
        })
}

/// The first letter of a sentence: the word at the paragraph's start, or
/// after `.`, `!` or `?` that does not end a First Letter exception.
fn capital_fix(
    text: &[char],
    start: usize,
    end: usize,
    opts: &AutoCorrectOptions,
    list: &dyn WordFixes,
) -> Option<Fix> {
    if !opts.capitalize_sentences {
        return None;
    }
    let (a, _) = core(text, start, end)?;
    let token: String = text[start..end].iter().collect();
    // An address is not a word: `www.x.com` stays as typed.
    if token.contains("://") || token.contains('@') || token.to_lowercase().starts_with("www.") {
        return None;
    }
    let first = text[a];
    if !first.is_lowercase() || !starts_sentence(&text[..start], list) {
        return None;
    }
    Some(Fix {
        name: AUTOCORRECT,
        start: a,
        end: a + 1,
        with: first.to_uppercase().collect(),
    })
}

/// Does a word after `before` start a sentence?
fn starts_sentence(before: &[char], list: &dyn WordFixes) -> bool {
    let mut end = before.len();
    while end > 0 && before[end - 1].is_whitespace() {
        end -= 1;
    }
    if end == 0 {
        return true;
    }
    let start = token_start(before, end);
    let mut prev: String = before[start..end].iter().collect();
    // The closing marks after the period: `"Stop." he` reads `Stop.`.
    while prev.ends_with(['"', '\'', ')', ']', '\u{201d}', '\u{2019}']) {
        prev.pop();
    }
    prev.ends_with(['.', '!', '?']) && !list.first_letter_exception(&prev)
}

/// The letters-and-digits core of `text[start..end]`, without the
/// punctuation around it; `None` when it has none.
fn core(text: &[char], start: usize, end: usize) -> Option<(usize, usize)> {
    let a = (start..end).find(|&i| text[i].is_alphanumeric())?;
    let b = (a..end).rev().find(|&i| text[i].is_alphanumeric())? + 1;
    Some((a, b))
}

/// `"` and `'` as curly quotes: opening at the start, after whitespace, an
/// opening bracket or quote, or a dash; closing (the apostrophe for `'`)
/// after anything else.
fn smart_quote(text: &[char], caret: usize, ch: char, opts: &AutoCorrectOptions) -> Option<Fix> {
    if !opts.smart_quotes {
        return None;
    }
    let opening = caret < 2 || {
        let prev = text[caret - 2];
        prev.is_whitespace()
            || matches!(
                prev,
                '(' | '[' | '{' | '<' | '-' | '\u{2013}' | '\u{2014}' | '\u{201c}' | '\u{2018}'
            )
    };
    let with = match (ch, opening) {
        ('"', true) => '\u{201c}',
        ('"', false) => '\u{201d}',
        (_, true) => '\u{2018}',
        (_, false) => '\u{2019}',
    };
    Some(Fix {
        name: AUTOFORMAT,
        start: caret - 1,
        end: caret,
        with: with.into(),
    })
}

/// `with` in the case `typed` shows: all capitals for an all-capitals word
/// of two or more letters, an initial capital for a capitalised one.
fn match_case(typed: &str, with: &str) -> String {
    let letters: Vec<char> = typed.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase()) {
        return with.to_uppercase();
    }
    if letters.first().is_some_and(|c| c.is_uppercase()) {
        let mut cs = with.chars();
        return match cs.next() {
            Some(c) => c.to_uppercase().chain(cs).collect(),
            None => String::new(),
        };
    }
    with.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `typed` one key at a time with every fix applied, as the editor does.
    fn type_all(typed: &str, opts: &AutoCorrectOptions) -> String {
        let mut text: Vec<char> = Vec::new();
        for ch in typed.chars() {
            text.push(ch);
            let caret = text.len();
            for fix in fixes_for(&text, caret, ch, opts, &BuiltinFixes) {
                fix.apply(&mut text);
            }
        }
        text.into_iter().collect()
    }

    fn typed(s: &str) -> String {
        type_all(s, &AutoCorrectOptions::default())
    }

    #[test]
    fn replacements_and_capitals() {
        assert_eq!(typed("teh "), "The ");
        assert_eq!(typed("Then teh "), "Then the ");
        assert_eq!(typed("TEH "), "THE ");
        assert_eq!(typed("so (teh) "), "So (the) ");
        assert_eq!(typed("hello. world "), "Hello. World ");
        assert_eq!(typed("see e.g. this "), "See e.g. this ");
        assert_eq!(typed("he said \"stop.\" then "), "He said “stop.” Then ");
        assert_eq!(typed("www.example.com "), "www.example.com ");
        assert_eq!(typed("5 apples "), "5 apples ");
    }

    #[test]
    fn two_fixes_on_one_key_come_in_word_order() {
        let mut text: Vec<char> = "teh ".chars().collect();
        let fixes = fixes_for(&text, 4, ' ', &AutoCorrectOptions::default(), &BuiltinFixes);
        let names: Vec<_> = fixes.iter().map(|f| (f.name, f.with.as_str())).collect();
        assert_eq!(names, [(AUTOCORRECT, "the"), (AUTOCORRECT, "T")]);
        for fix in fixes {
            fix.apply(&mut text);
        }
        assert_eq!(text.iter().collect::<String>(), "The ");
    }

    #[test]
    fn a_capital_follows_a_replacement_of_several_words() {
        let mut text: Vec<char> = "alot ".chars().collect();
        let fixes = fixes_for(&text, 5, ' ', &AutoCorrectOptions::default(), &BuiltinFixes);
        let names: Vec<_> = fixes.iter().map(|f| (f.name, f.with.as_str())).collect();
        assert_eq!(names, [(AUTOCORRECT, "a lot"), (AUTOCORRECT, "A")]);
        for fix in fixes {
            fix.apply(&mut text);
        }
        assert_eq!(text.iter().collect::<String>(), "A lot ");
        assert_eq!(typed("Yes. alot "), "Yes. A lot ");
    }

    /// A list whose one entry's With holds a field's stand-in.
    struct FieldWith;

    impl WordFixes for FieldWith {
        fn replacement(&self, token: &str) -> Option<String> {
            (token == "xx").then(|| format!("{}ab", crate::editor::FIELD_CHAR))
        }

        fn first_letter_exception(&self, _: &str) -> bool {
            false
        }
    }

    #[test]
    fn a_with_never_carries_a_field_stand_in() {
        let text: Vec<char> = "xx ".chars().collect();
        let fixes = fixes_for(&text, 3, ' ', &AutoCorrectOptions::default(), &FieldWith);
        let withs: Vec<_> = fixes.iter().map(|f| f.with.as_str()).collect();
        assert_eq!(withs, ["ab", "A"]);
    }

    #[test]
    fn symbols_fire_on_the_key_that_completes_them() {
        assert_eq!(typed("Mark (c)"), "Mark \u{a9}");
        assert_eq!(typed("Wait..."), "Wait\u{2026}");
        assert_eq!(typed("A --> "), "A \u{2192} ");
        // `)` that completes no symbol runs the word rules.
        assert_eq!(typed("A (teh)"), "A (the)");
    }

    #[test]
    fn quotes_and_dashes() {
        assert_eq!(typed("He said \"hi\" ok"), "He said “hi” ok");
        assert_eq!(typed("It's 'odd'"), "It’s ‘odd’");
        assert_eq!(typed("(\"x\")"), "(“x”)");
        assert_eq!(typed("One--two "), "One\u{2014}two ");
        assert_eq!(typed("One -- two "), "One -- two ");
        assert_eq!(typed("a---b "), "A---b ");
    }

    #[test]
    fn each_switch_turns_its_rule_off() {
        let off = |f: fn(&mut AutoCorrectOptions)| {
            let mut o = AutoCorrectOptions::default();
            f(&mut o);
            o
        };
        assert_eq!(type_all("teh ", &off(|o| o.replace_text = false)), "Teh ");
        assert_eq!(
            type_all("Mark (c)", &off(|o| o.replace_text = false)),
            "Mark (c)"
        );
        assert_eq!(
            type_all("teh ", &off(|o| o.capitalize_sentences = false)),
            "the "
        );
        assert_eq!(
            type_all("\"hi\"", &off(|o| o.smart_quotes = false)),
            "\"hi\""
        );
        assert_eq!(
            type_all("One--two ", &off(|o| o.dashes = false)),
            "One--two "
        );
        let lists_off = off(|o| o.auto_lists = false);
        assert_eq!(auto_list(&['1', '.', ' '], 3, ' ', false, &lists_off), None);
    }

    #[test]
    fn automatic_lists() {
        let o = AutoCorrectOptions::default();
        let t = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(
            auto_list(&t("1. "), 3, ' ', false, &o),
            Some(ListKind::Decimal)
        );
        assert_eq!(
            auto_list(&t("* "), 2, ' ', false, &o),
            Some(ListKind::Bullet)
        );
        assert_eq!(
            auto_list(&t("- "), 2, ' ', false, &o),
            Some(ListKind::Bullet)
        );
        assert_eq!(
            auto_list(&t("1) "), 3, ' ', false, &o),
            Some(ListKind::Decimal)
        );
        assert_eq!(auto_list(&t("1. "), 3, ' ', true, &o), None);
        assert_eq!(auto_list(&t("- "), 2, ' ', true, &o), None);
        assert_eq!(auto_list(&t("1) "), 3, ' ', true, &o), None);
        for s in ["-- ", "x- ", "a 1) ", "1)5 ", "1) x", "- x", "-x "] {
            assert_eq!(auto_list(&t(s), t(s).len(), ' ', false, &o), None, "{s}");
        }
        assert_eq!(auto_list(&t("- "), 1, ' ', false, &o), None);
        let off = AutoCorrectOptions {
            auto_lists: false,
            ..AutoCorrectOptions::default()
        };
        assert_eq!(auto_list(&t("- "), 2, ' ', false, &off), None);
        assert_eq!(auto_list(&t("1) "), 3, ' ', false, &off), None);
        assert_eq!(auto_list(&t("1.5 "), 4, ' ', false, &o), None);
        assert_eq!(auto_list(&t("1. x"), 3, ' ', false, &o), None);
    }
}
