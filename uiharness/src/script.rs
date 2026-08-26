//! The format a UI test is written in, and its parser.
//!
//! A script is a plain text file of cases. Each case names itself, drives the
//! app with the verbs of `suite/docxy/src/harness.rs`, and states what it
//! expects — of the picture, in the vocabulary of [`crate::expect`], and of the
//! app's own reported state.
//!
//! ```text
//! # sales.uit — the drag-to-select regressions
//!
//! test drag-to-select does not fill
//!   open fixtures/basic.xlsx
//!   drag A1 -> C5
//!   assert range is A1:C5
//!   assert no fill preview
//!   shot cell:A1:C5
//!   assert border A1:C5 solid teal
//! ```
//!
//! [`parse_script`] is a **pure function over the text**: no app, no window, no
//! files. That is deliberate and is what the plan's "keep the decisions in pure
//! free functions" means here — every accepted and rejected form is a unit
//! test, and a script with a typo in it fails in milliseconds rather than after
//! a cold `suite.exe` has drawn its first frame.
//!
//! ## What the parser refuses, and why it refuses it here
//!
//! Region names are checked at parse time ([`validate_region`]), unlike the
//! single-shot `uiharness assert` path, which deliberately lets the app answer
//! for a typo'd region. The difference is what the reader is doing: a person
//! typing one assertion at a prompt gets the app's answer immediately, while a
//! script is a batch — finding out on step 9 of case 3 that step 1 named
//! `grd` costs a whole launch. So a script is checked whole, before anything
//! starts.
//!
//! ## Steps
//!
//! | Step | Drives |
//! |---|---|
//! | `open <path>` | the file, resolved against the script's own directory |
//! | `click <cell> [shift] [double]` | the cell's click handler |
//! | `drag <from> -> <to>` | press, one move per cell crossed, release |
//! | `type <text>` | one key event per character |
//! | `key <k> [k…]` | those keys, in order |
//! | `select chart <n>` | the press on a chart card |
//! | `focus <field>` | the click on a reference field |
//! | `snapshot <range>` | remembers those cells, for `assert cells unchanged` |
//! | `shot <region>` | files a PNG of the region |
//! | `assert …` | see below |
//!
//! ## Assertions
//!
//! | Assertion | Reads |
//! |---|---|
//! | `border A1:C5 solid teal` | the pixels ([`crate::expect`]) |
//! | `no border H20 teal` | the pixels |
//! | `<key> is [not] <value>` | one key of the app's state reply |
//! | `cell B2 is [not] <text>` | what that cell shows |
//! | `no fill preview` | `filling` and `fill_preview` together |
//! | `cells unchanged` | every cell of the last `snapshot` |
//!
//! `no fill preview` and `cells unchanged` are the two phrases from the plan's
//! own example script. They are not sugar for one state key: an auto-fill that
//! ran leaves `filling` false again afterwards and only the *cells* show it,
//! and a fill that is merely armed shows in `fill_preview` while no cell has
//! changed yet. Asking for one of those and not the other is how the original
//! regression got through.

use crate::expect::{BorderExpect, parse_border};

/// One parsed script.
#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    pub cases: Vec<Case>,
}

/// One test case: a name and the steps under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub name: String,
    /// The 1-based line its `test` header is on, for a message.
    pub line: usize,
    pub steps: Vec<Step>,
}

/// One step, with the line it came from so a failure can quote the script.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub line: usize,
    /// The step as written, comment and indentation stripped.
    pub source: String,
    pub action: Action,
}

/// What a step does.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Open a file. The path is as written; the runner resolves it against the
    /// script's directory, so a case cannot be made to depend on the working
    /// directory it was run from.
    Open(String),
    Click {
        cell: String,
        shift: bool,
        double: bool,
    },
    Drag {
        from: String,
        to: String,
    },
    Type(String),
    Key(Vec<String>),
    SelectChart(usize),
    FocusField(String),
    /// Remember the cells of a range, for a later `assert cells unchanged`.
    Snapshot {
        range: String,
        cells: Vec<String>,
    },
    /// File a PNG of a region.
    Shot(String),
    Assert(Assertion),
}

/// What a step asserts.
#[derive(Debug, Clone, PartialEq)]
pub enum Assertion {
    /// A border expectation, read from the region's pixels.
    Border(Box<BorderExpect>),
    /// One key of the app's state reply.
    State {
        key: String,
        negated: bool,
        value: String,
    },
    /// What a cell shows.
    Cell {
        cell: String,
        negated: bool,
        value: String,
    },
    /// No fill is armed and none is previewed.
    NoFillPreview,
    /// Every cell of the last `snapshot` still holds what it held.
    CellsUnchanged,
}

/// A parse failure, with the line it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    /// 1-based; 0 when the complaint is about the file as a whole.
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line == 0 {
            f.write_str(&self.message)
        } else {
            write!(f, "line {}: {}", self.line, self.message)
        }
    }
}

impl std::error::Error for ScriptError {}

fn err(line: usize, message: impl Into<String>) -> ScriptError {
    ScriptError {
        line,
        message: message.into(),
    }
}

/// The steps a script may use, for an error message.
const STEP_WORDS: &str =
    "open, click, drag, type, key, select chart, focus, snapshot, shot, assert";

/// Parse a whole script.
///
/// Pure. Blank lines are skipped and a `#` starts a comment to end of line —
/// except inside a `type` step, whose text is taken verbatim, because `#` is a
/// character a spreadsheet test has every reason to type.
pub fn parse_script(text: &str) -> Result<Script, ScriptError> {
    let mut cases: Vec<Case> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let (head, rest) = split_word(raw.trim());
        // `type` keeps its comment; everything else loses it. Decided from the
        // first word, before trimming, so the rule is one place.
        let verbatim = head.eq_ignore_ascii_case("type");
        let stripped = if verbatim {
            raw.trim().to_string()
        } else {
            strip_comment(raw).trim().to_string()
        };
        if stripped.is_empty() {
            continue;
        }
        let (head, rest) = if verbatim {
            (head, rest)
        } else {
            split_word(&stripped)
        };
        let (head, rest) = (head.to_string(), rest.to_string());

        if head.eq_ignore_ascii_case("test") {
            let name = rest.trim();
            if name.is_empty() {
                return Err(err(line, "'test' needs a name: 'test what it must do'"));
            }
            if cases.iter().any(|c| c.name == name) {
                return Err(err(
                    line,
                    format!(
                        "a second case is also called '{name}'; their captures would \
                         land on top of each other"
                    ),
                ));
            }
            cases.push(Case {
                name: name.to_string(),
                line,
                steps: Vec::new(),
            });
            continue;
        }

        let action = parse_step(&head, &rest, line)?;
        match cases.last_mut() {
            Some(c) => c.steps.push(Step {
                line,
                source: stripped,
                action,
            }),
            None => {
                return Err(err(
                    line,
                    format!("'{head}' is outside any case; start one with 'test <name>'"),
                ));
            }
        }
    }
    if cases.is_empty() {
        return Err(err(
            0,
            "this script has no cases; start one with 'test <name>'",
        ));
    }
    if let Some(c) = cases.iter().find(|c| c.steps.is_empty()) {
        return Err(err(c.line, format!("the case '{}' has no steps", c.name)));
    }
    Ok(Script { cases })
}

/// One step's line, past its first word.
fn parse_step(head: &str, rest: &str, line: usize) -> Result<Action, ScriptError> {
    let lower = head.to_ascii_lowercase();
    let rest_trim = rest.trim();
    match lower.as_str() {
        "open" => {
            if rest_trim.is_empty() {
                return Err(err(line, "'open' needs a path"));
            }
            Ok(Action::Open(unquote(rest_trim).to_string()))
        }

        "click" => {
            let mut words = rest_trim.split_whitespace();
            let cell = words
                .next()
                .ok_or_else(|| err(line, "'click' needs a cell, e.g. 'click B2'"))?;
            let cell = validate_cell(cell).map_err(|e| err(line, e))?;
            let (mut shift, mut double) = (false, false);
            for w in words {
                match w.to_ascii_lowercase().as_str() {
                    "shift" => shift = true,
                    "double" | "dbl" => double = true,
                    other => {
                        return Err(err(
                            line,
                            format!("'{other}' is not a click modifier (shift, double)"),
                        ));
                    }
                }
            }
            Ok(Action::Click {
                cell,
                shift,
                double,
            })
        }

        "drag" => {
            // `drag A1 -> C5`, `drag A1 to C5` and `drag A1 C5` all read the
            // same way to someone skimming; the arrow is the one the plan's own
            // example uses.
            let words: Vec<&str> = rest_trim
                .split_whitespace()
                .filter(|w| *w != "->" && !w.eq_ignore_ascii_case("to"))
                .collect();
            if words.len() != 2 {
                return Err(err(
                    line,
                    "'drag' needs two cells: 'drag A1 -> C5'".to_string(),
                ));
            }
            Ok(Action::Drag {
                from: validate_cell(words[0]).map_err(|e| err(line, e))?,
                to: validate_cell(words[1]).map_err(|e| err(line, e))?,
            })
        }

        // Verbatim: leading spaces are already gone, everything else is typed.
        "type" => {
            if rest.is_empty() {
                return Err(err(line, "'type' needs something to type"));
            }
            Ok(Action::Type(rest.to_string()))
        }

        "key" => {
            let keys: Vec<String> = rest_trim.split_whitespace().map(str::to_string).collect();
            if keys.is_empty() {
                return Err(err(line, "'key' needs a key, e.g. 'key escape'"));
            }
            Ok(Action::Key(keys))
        }

        // `select chart 0`, and `select-chart 0` for anyone who typed the verb.
        "select" | "select-chart" => {
            let mut words = rest_trim.split_whitespace().peekable();
            if lower == "select" {
                match words.next() {
                    Some(w) if w.eq_ignore_ascii_case("chart") => {}
                    Some(w) => {
                        return Err(err(
                            line,
                            format!(
                                "'select {w}' is not a step; the only one is 'select chart <n>'"
                            ),
                        ));
                    }
                    None => return Err(err(line, "'select' needs 'chart <n>'")),
                }
            }
            let n = words
                .next()
                .ok_or_else(|| err(line, "'select chart' needs an index; they count from 0"))?;
            let idx: usize = n.parse().map_err(|_| {
                err(
                    line,
                    format!("'{n}' is not a chart index (they count from 0)"),
                )
            })?;
            if let Some(extra) = words.next() {
                return Err(err(
                    line,
                    format!("'{extra}' is not part of 'select chart'"),
                ));
            }
            Ok(Action::SelectChart(idx))
        }

        "focus" | "focus-field" => {
            if rest_trim.is_empty() {
                return Err(err(
                    line,
                    "'focus' needs a field, e.g. 'focus chart-range'".to_string(),
                ));
            }
            Ok(Action::FocusField(rest_trim.to_string()))
        }

        "snapshot" => {
            if rest_trim.is_empty() {
                return Err(err(line, "'snapshot' needs a range, e.g. 'snapshot A1:C5'"));
            }
            let cells = cells_in_range(rest_trim).map_err(|e| err(line, e))?;
            Ok(Action::Snapshot {
                range: rest_trim.to_string(),
                cells,
            })
        }

        "shot" => {
            if rest_trim.is_empty() {
                return Err(err(line, format!("'shot' needs a region ({REGION_WORDS})")));
            }
            Ok(Action::Shot(
                validate_region(rest_trim).map_err(|e| err(line, e))?,
            ))
        }

        "assert" => Ok(Action::Assert(parse_assertion(rest_trim, line)?)),

        other => Err(err(line, format!("'{other}' is not a step ({STEP_WORDS})"))),
    }
}

/// One assertion's text, past the `assert`.
fn parse_assertion(text: &str, line: usize) -> Result<Assertion, ScriptError> {
    let t = text.trim();
    if t.is_empty() {
        return Err(err(
            line,
            "'assert' needs an expectation (border …, <key> is <value>, \
             cell <ref> is <text>, no fill preview, cells unchanged)",
        ));
    }
    let lower = t.to_ascii_lowercase();

    // The two phrases from the plan's own example script.
    if lower == "no fill preview" {
        return Ok(Assertion::NoFillPreview);
    }
    if lower == "cells unchanged" {
        return Ok(Assertion::CellsUnchanged);
    }

    // Anything about a border goes to the vocabulary that already exists.
    if lower.starts_with("border ") || lower.starts_with("no border ") {
        let exp = parse_border(t).map_err(|e| err(line, e))?;
        // The one thing `parse_border` deliberately leaves to the app; a script
        // is checked whole before it starts, so it is caught here instead.
        validate_region(&exp.region).map_err(|e| err(line, e))?;
        return Ok(Assertion::Border(Box::new(exp)));
    }

    // `cell B2 is 12` / `<key> is [not] <value>`.
    let (subject, rest) = split_word(t);
    if subject.eq_ignore_ascii_case("cell") {
        let (cell, rest) = split_word(rest.trim());
        let cell = validate_cell(cell).map_err(|e| err(line, e))?;
        let (negated, value) = parse_is(rest, line, t)?;
        return Ok(Assertion::Cell {
            cell,
            negated,
            value,
        });
    }
    if subject.is_empty() {
        return Err(err(line, format!("'{t}' is not an expectation")));
    }
    let (negated, value) = parse_is(rest, line, t)?;
    Ok(Assertion::State {
        key: subject.to_string(),
        negated,
        value,
    })
}

/// The `is [not] <value>` tail the state and cell assertions share.
fn parse_is(rest: &str, line: usize, whole: &str) -> Result<(bool, String), ScriptError> {
    let (word, rest) = split_word(rest.trim());
    if !word.eq_ignore_ascii_case("is") {
        return Err(err(
            line,
            format!(
                "'{whole}' is not an expectation; write '<key> is <value>' \
                 (or 'border …', 'no fill preview', 'cells unchanged')"
            ),
        ));
    }
    let mut value = rest.trim();
    let mut negated = false;
    let (maybe_not, after_not) = split_word(value);
    if maybe_not.eq_ignore_ascii_case("not") {
        negated = true;
        value = after_not.trim();
    }
    if value.is_empty() {
        return Err(err(
            line,
            format!("'{whole}' does not say what to expect after 'is'"),
        ));
    }
    Ok((negated, unquote(value).to_string()))
}

// ---------------------------------------------------------------------------
// The pure helpers a step is validated with
// ---------------------------------------------------------------------------

/// The region names, for an error message. Mirrors `harness::parse_region`.
const REGION_WORDS: &str = "window, grid, chart-panel, cell:B3, cell:A1:C5, chart:0";

/// A region name a script may use, normalized to the form the app's `rect`
/// verb takes (`A1:C5` becomes `cell:A1:C5`).
///
/// This mirrors `suite/docxy/src/harness.rs`'s `parse_region`, which is the
/// authority; the copy exists because the two crates are in different
/// workspaces, and it only has to reject what that one would — a script that
/// passed here and failed there would still fail, just later and worse.
pub fn validate_region(name: &str) -> Result<String, String> {
    let full = crate::expect::normalize_region(name);
    let (head, arg) = match full.split_once(':') {
        Some((h, a)) => (h.trim(), Some(a.trim())),
        None => (full.as_str(), None),
    };
    match head.to_ascii_lowercase().as_str() {
        "window" | "grid" | "chart-panel" => {
            if arg.is_some() {
                return Err(format!("'{head}' takes no argument"));
            }
            Ok(full.clone())
        }
        "cell" | "cells" => {
            let a = arg
                .filter(|a| !a.is_empty())
                .ok_or_else(|| format!("'{head}' needs a cell or a range, e.g. {head}:B3"))?;
            match a.split_once(':') {
                Some((s, e)) => {
                    validate_cell(s)?;
                    validate_cell(e)?;
                }
                None => {
                    validate_cell(a)?;
                }
            }
            Ok(full.clone())
        }
        "chart" => {
            let a = arg
                .filter(|a| !a.is_empty())
                .ok_or_else(|| "'chart' needs an index, e.g. chart:0".to_string())?;
            a.parse::<usize>()
                .map_err(|_| format!("'{a}' is not a chart index (they count from 0)"))?;
            Ok(full.clone())
        }
        other => Err(format!("unknown region '{other}' ({REGION_WORDS})")),
    }
}

/// An A1-style cell reference, upper-cased. `$` is accepted and dropped: a
/// test author pasting a reference out of the formula bar should not have to
/// think about it.
pub fn validate_cell(text: &str) -> Result<String, String> {
    let t: String = text.trim().chars().filter(|c| *c != '$').collect();
    let up = t.to_ascii_uppercase();
    match parse_a1(&up) {
        Some(_) => Ok(up),
        None => Err(format!(
            "'{}' is not a cell reference, e.g. B3",
            text.trim()
        )),
    }
}

/// `B3` as 0-based `(row, col)`. `None` for anything that is not a plain A1
/// reference — the same shape `gridcore::sheet::parse_cell_name` takes, kept
/// here because this crate does not depend on gridcore.
pub fn parse_a1(text: &str) -> Option<(u32, u32)> {
    let t = text.trim();
    let letters: String = t.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits = &t[letters.len()..];
    if letters.is_empty() || digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // Excel's bijective base 26; anything past the sheet's own limits is the
    // app's business, not the parser's.
    let mut col: u32 = 0;
    for c in letters.chars() {
        col = col
            .checked_mul(26)?
            .checked_add(c.to_ascii_uppercase() as u32 - 'A' as u32 + 1)?;
    }
    let row: u32 = digits.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

/// `A1` for a 0-based `(row, col)`.
pub fn a1_name(row: u32, col: u32) -> String {
    let mut n = col + 1;
    let mut letters = String::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        letters.insert(0, (b'A' + rem) as char);
        n = (n - 1) / 26;
    }
    format!("{letters}{}", row + 1)
}

/// The cells of `A1:C5`, in reading order — what `snapshot` remembers.
///
/// Capped, because `snapshot A1:XFD1048576` would ask the app for ten billion
/// cells one round trip at a time and look like a hang.
pub fn cells_in_range(text: &str) -> Result<Vec<String>, String> {
    let t = text.trim();
    let (a, b) = match t.split_once(':') {
        Some((a, b)) => (a, b),
        None => (t, t),
    };
    let (r0, c0) = parse_a1(&validate_cell(a)?).ok_or_else(|| format!("'{a}' is not a cell"))?;
    let (r1, c1) = parse_a1(&validate_cell(b)?).ok_or_else(|| format!("'{b}' is not a cell"))?;
    let (rlo, rhi) = (r0.min(r1), r0.max(r1));
    let (clo, chi) = (c0.min(c1), c0.max(c1));
    let count = (rhi - rlo + 1) as u64 * (chi - clo + 1) as u64;
    const MAX: u64 = 2000;
    if count > MAX {
        return Err(format!(
            "'{t}' is {count} cells; a snapshot reads them one at a time, so it is \
             capped at {MAX}"
        ));
    }
    let mut out = Vec::with_capacity(count as usize);
    for r in rlo..=rhi {
        for c in clo..=chi {
            out.push(a1_name(r, c));
        }
    }
    Ok(out)
}

/// The first whitespace-separated word, and the rest of the line untrimmed —
/// the untrimmed tail is what lets `type` take its text verbatim.
fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    // ⚠️ Split by the separator's own length, not by 1. `char::is_whitespace` is
    // true for U+00A0 and U+3000 as well as for a space, and those are two and
    // three bytes in UTF-8 — slicing at `i + 1` lands inside the character and
    // panics. A non-breaking space is what pasting a step out of a document or
    // a browser leaves behind, and the parser's whole contract is that a bad
    // line comes back as a message with a line number on it.
    match s.char_indices().find(|(_, c)| c.is_whitespace()) {
        Some((i, c)) => (&s[..i], &s[i + c.len_utf8()..]),
        None => (s, ""),
    }
}

/// Everything before an unquoted `#` that is not the start of a colour.
///
/// ⚠️ `#rrggbb` is a colour, and `assert border A1:B4 solid #2f6fdb` was
/// silently becoming `assert border A1:B4 solid` — an assertion that still
/// passed, about a weaker thing than the case said. Caught by reading the
/// runner's transcript of a case that passed, which is the argument for
/// printing every step rather than only the failures.
fn strip_comment(line: &str) -> &str {
    let mut in_quote = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => in_quote = !in_quote,
            '#' if !in_quote && !starts_hex_color(&line[i + 1..]) => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Whether `rest` opens with exactly six hex digits followed by a word break —
/// the tail of a `#rrggbb`.
fn starts_hex_color(rest: &str) -> bool {
    let mut it = rest.chars();
    for _ in 0..6 {
        match it.next() {
            Some(c) if c.is_ascii_hexdigit() => {}
            _ => return false,
        }
    }
    !it.next().is_some_and(|c| c.is_ascii_alphanumeric())
}

/// A `"quoted value"` without its quotes; anything else untouched.
fn unquote(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        &t[1..t.len() - 1]
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expect::ExpectKind;

    const VALID: &str = "\
# the drag-to-select regressions
test drag-to-select does not fill
  open fixtures/basic.xlsx
  snapshot A1:C5
  drag A1 -> C5           # a sweep, not a fill
  assert range is A1:C5
  assert no fill preview
  assert cells unchanged
  shot cell:A1:C5
  assert border A1:C5 solid teal

test a pointed range dashes
  click E2
  type =SUM(
  assert editing is true
  drag A1 -> D5
  assert border A1:D5 dashed teal
";

    #[test]
    fn a_valid_script_parses_into_its_cases_and_steps() {
        let s = parse_script(VALID).unwrap();
        assert_eq!(s.cases.len(), 2);
        assert_eq!(s.cases[0].name, "drag-to-select does not fill");
        assert_eq!(s.cases[1].name, "a pointed range dashes");
        assert_eq!(s.cases[0].steps.len(), 8);
        assert_eq!(
            s.cases[0].steps[0].action,
            Action::Open("fixtures/basic.xlsx".to_string())
        );
        assert_eq!(
            s.cases[0].steps[2].action,
            Action::Drag {
                from: "A1".to_string(),
                to: "C5".to_string()
            },
            "the trailing comment is not part of the step"
        );
        assert_eq!(
            s.cases[0].steps[3].action,
            Action::Assert(Assertion::State {
                key: "range".to_string(),
                negated: false,
                value: "A1:C5".to_string()
            })
        );
        assert_eq!(
            s.cases[0].steps[4].action,
            Action::Assert(Assertion::NoFillPreview)
        );
        assert_eq!(
            s.cases[0].steps[5].action,
            Action::Assert(Assertion::CellsUnchanged)
        );
        assert_eq!(
            s.cases[0].steps[6].action,
            Action::Shot("cell:A1:C5".to_string()),
            "a bare range is the app's cell: region"
        );
        // The line numbers are the script's own, so a failure can be found.
        assert_eq!(s.cases[0].line, 2);
        assert_eq!(s.cases[0].steps[0].line, 3);
    }

    #[test]
    fn a_border_assertion_keeps_the_vocabulary_it_already_has() {
        let s = parse_script(VALID).unwrap();
        let Action::Assert(Assertion::Border(b)) = &s.cases[0].steps[7].action else {
            panic!("{:?}", s.cases[0].steps[7].action);
        };
        assert_eq!(b.region, "cell:A1:C5");
        assert_eq!(b.kind, ExpectKind::Solid);
        assert_eq!(b.color_label.as_deref(), Some("teal"));
    }

    /// `type` takes its line verbatim: a `#` is a character, not a comment.
    #[test]
    fn type_keeps_everything_after_the_word() {
        let s = parse_script("test t\n  type =SUM(A1:C5) # 1\n").unwrap();
        assert_eq!(
            s.cases[0].steps[0].action,
            Action::Type("=SUM(A1:C5) # 1".to_string())
        );
    }

    /// A `#rrggbb` colour is not a comment. It was, once, and the assertion
    /// that lost its colour still passed — about less than it said.
    #[test]
    fn a_hex_colour_survives_comment_stripping() {
        let s =
            parse_script("test t\n  assert border A1:B4 solid #2f6fdb   # the ref wash\n").unwrap();
        let Action::Assert(Assertion::Border(b)) = &s.cases[0].steps[0].action else {
            panic!("{:?}", s.cases[0].steps[0].action)
        };
        assert_eq!(b.color, Some([0x2f, 0x6f, 0xdb, 0xff]));
        assert_eq!(b.color_label.as_deref(), Some("#2f6fdb"));
        assert_eq!(
            s.cases[0].steps[0].source,
            "assert border A1:B4 solid #2f6fdb"
        );
        // A word that merely starts with a `#` is still a comment.
        assert!(
            strip_comment("shot grid #2f6fdbish")
                .trim()
                .ends_with("grid")
        );
        assert!(strip_comment("shot grid # note").trim().ends_with("grid"));
        assert!(strip_comment("shot grid #notes").trim().ends_with("grid"));
    }

    #[test]
    fn an_unknown_step_names_the_ones_there_are() {
        let e = parse_script("test t\n  frobnicate A1\n").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.message.contains("frobnicate"), "{e}");
        assert!(e.message.contains("assert"), "{e}");
    }

    #[test]
    fn a_malformed_assertion_says_what_the_shapes_are() {
        let e = parse_script("test t\n  assert range A1:C5\n").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.message.contains("is"), "{e}");
        // An assertion with nothing after `is`.
        let e = parse_script("test t\n  assert range is\n").unwrap_err();
        assert!(e.message.contains("after 'is'"), "{e}");
        // A border assertion that does not say what the border should be.
        let e = parse_script("test t\n  assert border A1:C5 teal\n").unwrap_err();
        assert!(e.message.contains("solid"), "{e}");
        // And an empty one.
        let e = parse_script("test t\n  assert\n").unwrap_err();
        assert!(e.message.contains("expectation"), "{e}");
    }

    #[test]
    fn a_step_naming_a_region_that_does_not_exist_is_refused_before_anything_launches() {
        let e = parse_script("test t\n  shot grd\n").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.message.contains("unknown region 'grd'"), "{e}");
        assert!(e.message.contains("chart-panel"), "{e}");
        // Inside an assertion too — the path that goes through `parse_border`.
        let e = parse_script("test t\n  assert border sidebar solid\n").unwrap_err();
        assert!(e.message.contains("unknown region 'sidebar'"), "{e}");
        // A region whose argument is wrong, rather than its name.
        let e = parse_script("test t\n  shot chart:left\n").unwrap_err();
        assert!(e.message.contains("chart index"), "{e}");
        let e = parse_script("test t\n  shot cell:\n").unwrap_err();
        assert!(e.message.contains("needs a cell"), "{e}");
    }

    #[test]
    fn a_step_outside_a_case_and_a_case_with_no_steps_are_both_refused() {
        let e = parse_script("open a.xlsx\n").unwrap_err();
        assert!(e.message.contains("outside any case"), "{e}");
        let e = parse_script("test empty\n").unwrap_err();
        assert!(e.message.contains("no steps"), "{e}");
        let e = parse_script("# nothing but a comment\n").unwrap_err();
        assert_eq!(e.line, 0);
        assert!(e.message.contains("no cases"), "{e}");
        let e = parse_script("test one\n  click A1\ntest one\n  click A1\n").unwrap_err();
        assert!(e.message.contains("also called"), "{e}");
    }

    /// A non-breaking space is what pasting a step out of a document, a chat
    /// window or a browser leaves behind, and it is invisible in an editor. The
    /// parser used to slice one byte past the separator, which lands inside a
    /// multi-byte space and panics — so a typo took the process down instead of
    /// coming back with a line number on it.
    #[test]
    fn a_non_ascii_space_separates_words_like_any_other_and_never_panics() {
        for gap in ["\u{a0}", "\u{3000}", "\u{2009}"] {
            let s = parse_script(&format!("test t\n  click{gap}A1\n")).unwrap();
            assert_eq!(
                s.cases[0].steps[0].action,
                Action::Click {
                    cell: "A1".to_string(),
                    shift: false,
                    double: false
                },
                "{gap:?}"
            );
        }
        // The same character inside a word is reported, not panicked on.
        let e = parse_script("test t\n  cli\u{a0}ck A1\n").unwrap_err();
        assert!(e.message.contains("cli"), "{e}");
        // A step that is nothing but one is an empty line, not a step.
        let e = parse_script("test t\n  \u{a0}\n").unwrap_err();
        assert!(e.message.contains("no steps"), "{e}");
    }

    #[test]
    fn the_drag_arrow_is_optional_and_the_cells_are_checked() {
        for text in [
            "drag A1 -> C5",
            "drag A1 to C5",
            "drag A1 C5",
            "drag $A$1 C5",
        ] {
            let s = parse_script(&format!("test t\n  {text}\n")).unwrap();
            assert_eq!(
                s.cases[0].steps[0].action,
                Action::Drag {
                    from: "A1".to_string(),
                    to: "C5".to_string()
                },
                "{text}"
            );
        }
        let e = parse_script("test t\n  drag A1\n").unwrap_err();
        assert!(e.message.contains("two cells"), "{e}");
        let e = parse_script("test t\n  drag A1 -> zz\n").unwrap_err();
        assert!(e.message.contains("not a cell reference"), "{e}");
    }

    #[test]
    fn click_takes_its_modifiers_and_refuses_the_ones_it_does_not_have() {
        let s = parse_script("test t\n  click B2 shift double\n").unwrap();
        assert_eq!(
            s.cases[0].steps[0].action,
            Action::Click {
                cell: "B2".to_string(),
                shift: true,
                double: true
            }
        );
        let e = parse_script("test t\n  click B2 ctrl\n").unwrap_err();
        assert!(e.message.contains("not a click modifier"), "{e}");
    }

    #[test]
    fn select_chart_takes_an_index_either_way_it_is_written() {
        for text in ["select chart 0", "select-chart 0"] {
            let s = parse_script(&format!("test t\n  {text}\n")).unwrap();
            assert_eq!(s.cases[0].steps[0].action, Action::SelectChart(0), "{text}");
        }
        let e = parse_script("test t\n  select chart first\n").unwrap_err();
        assert!(e.message.contains("chart index"), "{e}");
        let e = parse_script("test t\n  select cell\n").unwrap_err();
        assert!(e.message.contains("select chart"), "{e}");
    }

    #[test]
    fn a_negated_state_or_cell_assertion_parses() {
        let s = parse_script("test t\n  assert editing is not true\n  assert cell B2 is not 12\n")
            .unwrap();
        assert_eq!(
            s.cases[0].steps[0].action,
            Action::Assert(Assertion::State {
                key: "editing".to_string(),
                negated: true,
                value: "true".to_string()
            })
        );
        assert_eq!(
            s.cases[0].steps[1].action,
            Action::Assert(Assertion::Cell {
                cell: "B2".to_string(),
                negated: true,
                value: "12".to_string()
            })
        );
    }

    /// A value with spaces in it needs quotes, and the quotes are not part of
    /// the value.
    #[test]
    fn a_quoted_value_keeps_its_spaces_and_loses_its_quotes() {
        let s = parse_script("test t\n  assert cell A1 is \"North West\"\n").unwrap();
        assert_eq!(
            s.cases[0].steps[0].action,
            Action::Assert(Assertion::Cell {
                cell: "A1".to_string(),
                negated: false,
                value: "North West".to_string()
            })
        );
        // And a `#` inside quotes is not a comment.
        let s = parse_script("test t\n  assert cell A1 is \"#DIV/0!\"\n").unwrap();
        let Action::Assert(Assertion::Cell { value, .. }) = &s.cases[0].steps[0].action else {
            panic!()
        };
        assert_eq!(value, "#DIV/0!");
    }

    #[test]
    fn snapshot_enumerates_the_range_and_refuses_an_unreadable_one() {
        let s = parse_script("test t\n  snapshot A1:B2\n").unwrap();
        let Action::Snapshot { range, cells } = &s.cases[0].steps[0].action else {
            panic!()
        };
        assert_eq!(range, "A1:B2");
        assert_eq!(cells, &["A1", "B1", "A2", "B2"]);
        // One cell is a range of one.
        let s = parse_script("test t\n  snapshot C7\n").unwrap();
        let Action::Snapshot { cells, .. } = &s.cases[0].steps[0].action else {
            panic!()
        };
        assert_eq!(cells, &["C7"]);
        let e = parse_script("test t\n  snapshot A1:XFD1048576\n").unwrap_err();
        assert!(e.message.contains("capped"), "{e}");
    }

    #[test]
    fn a1_names_and_references_round_trip() {
        for (name, rc) in [
            ("A1", (0, 0)),
            ("B3", (2, 1)),
            ("Z1", (0, 25)),
            ("AA1", (0, 26)),
            ("XFD1048576", (1_048_575, 16_383)),
        ] {
            assert_eq!(parse_a1(name), Some(rc), "{name}");
            assert_eq!(a1_name(rc.0, rc.1), name, "{name}");
        }
        for bad in ["", "1", "A", "A0", "1A", "A1B", "A 1"] {
            assert_eq!(parse_a1(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_regions_a_script_may_name_are_the_ones_the_app_answers_for() {
        for (written, normalized) in [
            ("window", "window"),
            ("grid", "grid"),
            ("chart-panel", "chart-panel"),
            ("cell:B3", "cell:B3"),
            ("B3", "cell:B3"),
            ("A1:C5", "cell:A1:C5"),
            ("cell:A1:C5", "cell:A1:C5"),
            ("chart:0", "chart:0"),
        ] {
            assert_eq!(validate_region(written).unwrap(), normalized, "{written}");
        }
        assert!(validate_region("grid:1").is_err(), "grid takes no argument");
    }
}
