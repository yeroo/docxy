//! Running a parsed script against a live instance.
//!
//! [`crate::script`] decides what a case *says*; this is what happens when it
//! is run. The split matters: everything that can be wrong about a script is
//! wrong before a window exists, and everything here is about the app — a verb
//! the app refused, a state key it does not report, a border that came out
//! dashed.
//!
//! ## Every step reports, passing or not
//!
//! A run prints a line per step, not just the failures. The plan's whole
//! complaint about the status quo is that "install a build and look at it" is
//! the only way to find out what happened, and a transcript that only mentions
//! the failure leaves the reader guessing whether the setup even worked.
//!
//! ```text
//! case: drag-to-select does not fill
//!   ok    open fixtures/basic.xlsx
//!   ok    drag A1 -> C5
//!   FAIL  assert no fill preview
//!         expected: no fill armed and nothing previewed
//!         observed: filling=true, fill_preview=A1:C5
//! ```
//!
//! ## The pure parts
//!
//! [`scalar_text`], [`value_matches`], [`fill_preview_verdict`] and
//! [`changed_cells`] are free functions over data the app already sent, so the
//! comparisons a case turns on are unit tests rather than something only a
//! window can exercise.

use crate::driver::Driver;
use crate::expect::{BorderExpect, check_border};
use crate::image::Image;
use crate::probe::ProbeOpts;
use crate::run::Run;
use crate::script::{Action, Assertion, Case, Script, Step};
use ctlcore::json::Json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How a step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// It did what it said.
    Ok,
    /// An expectation was not met — the app is wrong, or the case is.
    Failed,
    /// The step could not be run at all: the app refused the verb, the file was
    /// not there, the capture failed. Distinguished from `Failed` because it
    /// says nothing about the thing under test.
    Error,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Failed => "FAIL",
            Status::Error => "ERROR",
        }
    }
}

/// One step, run.
#[derive(Debug, Clone)]
pub struct StepOutcome {
    pub line: usize,
    pub source: String,
    pub status: Status,
    /// The lines under the step: the report of a border check, or what a state
    /// key actually said. Empty for a step that passed uneventfully.
    pub detail: String,
    /// A PNG this step filed, if it took one.
    pub evidence: Option<PathBuf>,
}

/// One case, run.
#[derive(Debug, Clone)]
pub struct CaseOutcome {
    pub name: String,
    pub steps: Vec<StepOutcome>,
}

impl CaseOutcome {
    pub fn passed(&self) -> bool {
        self.steps.iter().all(|s| s.status == Status::Ok)
    }

    /// The case's transcript.
    pub fn report(&self) -> String {
        let mut out = format!(
            "case: {} — {}\n",
            self.name,
            if self.passed() { "ok" } else { "FAILED" }
        );
        for s in &self.steps {
            out.push_str(&format!("  {:<6}{}\n", s.status.label(), s.source));
            for l in s.detail.lines() {
                out.push_str(&format!("        {l}\n"));
            }
            if let Some(p) = &s.evidence {
                out.push_str(&format!("        evidence: {}\n", p.display()));
            }
        }
        out
    }
}

/// A whole script, run.
#[derive(Debug, Clone)]
pub struct ScriptOutcome {
    pub cases: Vec<CaseOutcome>,
}

impl ScriptOutcome {
    pub fn passed(&self) -> bool {
        self.cases.iter().all(CaseOutcome::passed)
    }

    pub fn report(&self) -> String {
        let mut out = String::new();
        for c in &self.cases {
            out.push_str(&c.report());
            out.push('\n');
        }
        let failed = self.cases.iter().filter(|c| !c.passed()).count();
        out.push_str(&format!(
            "{} case{} — {} passed, {} failed\n",
            self.cases.len(),
            if self.cases.len() == 1 { "" } else { "s" },
            self.cases.len() - failed,
            failed
        ));
        out
    }
}

/// What a run needs besides the script: somewhere to file evidence, and where
/// a relative `open` path is relative to.
pub struct Runner<'a> {
    driver: &'a Driver,
    run: &'a Run,
    /// The script's own directory. A case's fixture path is resolved against
    /// this rather than the working directory, so `uiharness run` from anywhere
    /// opens the same file.
    base: PathBuf,
    opts: ProbeOpts,
}

impl<'a> Runner<'a> {
    pub fn new(driver: &'a Driver, run: &'a Run, base: impl Into<PathBuf>) -> Runner<'a> {
        Runner {
            driver,
            run,
            base: base.into(),
            opts: ProbeOpts::default(),
        }
    }

    /// Run every case, in order.
    ///
    /// The cases share one instance, so a later case sees what an earlier one
    /// did. That is on purpose — starting a process per case would multiply a
    /// two-second launch by however many cases there are — and it is why every
    /// case opens its own file rather than assuming one is open.
    pub fn run_script(&mut self, script: &Script) -> ScriptOutcome {
        ScriptOutcome {
            cases: script.cases.iter().map(|c| self.run_case(c)).collect(),
        }
    }

    /// Run one case. A step that could not run at all (`Error`) stops the case:
    /// the steps after it were written assuming it worked, and running them
    /// would report failures that are only that step's failure again.
    pub fn run_case(&mut self, case: &Case) -> CaseOutcome {
        let mut steps = Vec::new();
        let mut snapshot: BTreeMap<String, String> = BTreeMap::new();
        let mut snapshot_range = String::new();
        for step in &case.steps {
            let outcome = self.run_step(&case.name, step, &mut snapshot, &mut snapshot_range);
            let stop = outcome.status == Status::Error;
            steps.push(outcome);
            if stop {
                break;
            }
        }
        CaseOutcome {
            name: case.name.clone(),
            steps,
        }
    }

    fn run_step(
        &mut self,
        case: &str,
        step: &Step,
        snapshot: &mut BTreeMap<String, String>,
        snapshot_range: &mut String,
    ) -> StepOutcome {
        let mut out = StepOutcome {
            line: step.line,
            source: step.source.clone(),
            status: Status::Ok,
            detail: String::new(),
            evidence: None,
        };
        match &step.action {
            Action::Open(path) => {
                let full = self.base.join(path);
                if !full.is_file() {
                    return err(out, format!("no such file: {}", full.display()));
                }
                // Canonicalized, so the app resolves the same file this side
                // checked rather than one relative to its own directory.
                let full = full.canonicalize().unwrap_or(full);
                match self.driver.call(
                    "open",
                    Json::obj(vec![("path", Json::Str(path_arg(&full)))]),
                ) {
                    Ok(_) => out,
                    Err(e) => err(out, e),
                }
            }

            Action::Click {
                cell,
                shift,
                double,
            } => self.verb(
                out,
                "click-cell",
                Json::obj(vec![
                    ("cell", Json::Str(cell.clone())),
                    ("shift", Json::Bool(*shift)),
                    ("double", Json::Bool(*double)),
                ]),
            ),

            Action::Drag { from, to } => self.verb(
                out,
                "drag",
                Json::obj(vec![
                    ("from", Json::Str(from.clone())),
                    ("to", Json::Str(to.clone())),
                ]),
            ),

            Action::Type(text) => self.verb(
                out,
                "type",
                Json::obj(vec![("text", Json::Str(text.clone()))]),
            ),

            Action::Key(keys) => self.verb(
                out,
                "key",
                Json::obj(vec![(
                    "keys",
                    Json::Arr(keys.iter().map(|k| Json::Str(k.clone())).collect()),
                )]),
            ),

            Action::SelectChart(i) => self.verb(
                out,
                "select-chart",
                Json::obj(vec![("index", Json::Num(*i as f64))]),
            ),

            Action::FocusField(f) => self.verb(
                out,
                "focus-field",
                Json::obj(vec![("field", Json::Str(f.clone()))]),
            ),

            Action::Snapshot { range, cells } => {
                snapshot.clear();
                snapshot_range.clear();
                snapshot_range.push_str(range);
                for c in cells {
                    match self.cell_text(c) {
                        Ok(t) => {
                            snapshot.insert(c.clone(), t);
                        }
                        Err(e) => return err(out, e),
                    }
                }
                out.detail = format!("{} cells remembered", snapshot.len());
                out
            }

            Action::Shot(region) => match self.driver.shot(region) {
                Ok((img, cap)) => match self.save(case, out.line, region, &img) {
                    Ok(p) => {
                        out.detail = format!("{}x{} via {}", img.w, img.h, cap.how);
                        out.evidence = Some(p);
                        out
                    }
                    Err(e) => err(out, e),
                },
                Err(e) => err(out, e),
            },

            Action::Assert(a) => self.assert(case, out, a, snapshot, snapshot_range),
        }
    }

    /// Send a driving verb; a refusal is an `Error`, not a failed expectation.
    fn verb(&self, out: StepOutcome, verb: &str, args: Json) -> StepOutcome {
        match self.driver.call(verb, args) {
            Ok(_) => out,
            Err(e) => err(out, e),
        }
    }

    fn assert(
        &mut self,
        case: &str,
        out: StepOutcome,
        a: &Assertion,
        snapshot: &BTreeMap<String, String>,
        snapshot_range: &str,
    ) -> StepOutcome {
        match a {
            Assertion::Border(exp) => self.assert_border(case, out, exp),

            Assertion::State {
                key,
                negated,
                value,
            } => {
                let state = match self.state() {
                    Ok(s) => s,
                    Err(e) => return err(out, e),
                };
                let Some(got) = state.get(key) else {
                    return err(
                        out,
                        format!(
                            "the app reports no '{key}'; it reports {}",
                            keys_of(&state).join(", ")
                        ),
                    );
                };
                let got = scalar_text(got);
                if value_matches(&got, value, *negated) {
                    out
                } else {
                    fail(
                        out,
                        format!(
                            "expected: {key} is {}{value}\nobserved: {key} is {got}",
                            if *negated { "not " } else { "" }
                        ),
                    )
                }
            }

            Assertion::Cell {
                cell,
                negated,
                value,
            } => match self.cell_text(cell) {
                Err(e) => err(out, e),
                Ok(got) => {
                    if value_matches(&got, value, *negated) {
                        out
                    } else {
                        fail(
                            out,
                            format!(
                                "expected: {cell} is {}{value}\nobserved: {cell} is {}",
                                if *negated { "not " } else { "" },
                                if got.is_empty() {
                                    "empty".to_string()
                                } else {
                                    got
                                }
                            ),
                        )
                    }
                }
            },

            Assertion::NoFillPreview => {
                let state = match self.state() {
                    Ok(s) => s,
                    Err(e) => return err(out, e),
                };
                match fill_preview_verdict(&state) {
                    Ok(()) => out,
                    Err(e) => fail(out, e),
                }
            }

            Assertion::CellsUnchanged => {
                if snapshot.is_empty() {
                    return err(
                        out,
                        "nothing was remembered; put a 'snapshot <range>' before this".to_string(),
                    );
                }
                let mut now = BTreeMap::new();
                for c in snapshot.keys() {
                    match self.cell_text(c) {
                        Ok(t) => {
                            now.insert(c.clone(), t);
                        }
                        Err(e) => return err(out, e),
                    }
                }
                let changed = changed_cells(snapshot, &now);
                if changed.is_empty() {
                    out
                } else {
                    fail(
                        out,
                        format!(
                            "expected: {snapshot_range} unchanged since the snapshot\n\
                             observed: {}",
                            changed.join(", ")
                        ),
                    )
                }
            }
        }
    }

    fn assert_border(&mut self, case: &str, out: StepOutcome, exp: &BorderExpect) -> StepOutcome {
        let (img, cap) = match self.driver.shot(&exp.region) {
            Ok(v) => v,
            Err(e) => return err(out, e),
        };
        let path = match self.save(case, out.line, &exp.region, &img) {
            Ok(p) => p,
            Err(e) => return err(out, e),
        };
        let check = check_border(exp, &img, self.opts);
        let mut out = out;
        out.evidence = Some(path);
        if check.passed() {
            out.detail = format!("{} ({}x{} via {})", exp.describe(), img.w, img.h, cap.how);
            out
        } else {
            // The whole report, indented under the step: what was expected,
            // what each edge actually read, and in what colour.
            let body = check.report(None);
            let body = body.lines().skip(1).collect::<Vec<_>>().join("\n");
            out.status = Status::Failed;
            out.detail = body;
            out
        }
    }

    /// The app's state reply, for a state assertion.
    fn state(&self) -> Result<Json, String> {
        self.driver.call("selection", Json::Obj(Vec::new()))
    }

    /// What a cell shows.
    fn cell_text(&self, cell: &str) -> Result<String, String> {
        let j = self.driver.call(
            "cell",
            Json::obj(vec![("cell", Json::Str(cell.to_string()))]),
        )?;
        Ok(j.get("text").map(scalar_text).unwrap_or_default())
    }

    /// File a capture under the case that took it, named for the step as well
    /// as the region.
    ///
    /// ⚠️ The line number is not decoration. Two steps in a case may shoot or
    /// assert the same region — a `shot` after the assertion that failed is the
    /// obvious pair — and without it the second write lands on the first's
    /// file, so the `evidence:` line under the failure points at a picture
    /// taken later, of a window that had moved on.
    fn save(&self, case: &str, line: usize, region: &str, img: &Image) -> Result<PathBuf, String> {
        self.run
            .save(case, &format!("{line:03}-{region}"), img)
            .map_err(|e| format!("saving the capture: {e}"))
    }
}

fn err(mut out: StepOutcome, detail: impl Into<String>) -> StepOutcome {
    out.status = Status::Error;
    out.detail = detail.into();
    out
}

fn fail(mut out: StepOutcome, detail: impl Into<String>) -> StepOutcome {
    out.status = Status::Failed;
    out.detail = detail.into();
    out
}

/// A path as the `open` verb wants it: a plain string. Windows' extended-length
/// prefix, which `canonicalize` adds, is stripped — the app does open it, but
/// it turns up in the tab title and in every message about the file.
fn path_arg(p: &Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// The keys an object has, for "the app reports no 'x'".
fn keys_of(j: &Json) -> Vec<&str> {
    match j {
        Json::Obj(pairs) => pairs.iter().map(|(k, _)| k.as_str()).collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The comparisons, as pure functions
// ---------------------------------------------------------------------------

/// A JSON scalar as a test would have written it.
///
/// Numbers lose a trailing `.0`, because the app sends `2` as `2.0` and a case
/// that had to write `chart_sel is 0.0` would be describing the wire format
/// rather than the app.
pub fn scalar_text(j: &Json) -> String {
    match j {
        Json::Null => "null".to_string(),
        Json::Bool(b) => b.to_string(),
        Json::Num(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{n:.0}")
            } else {
                n.to_string()
            }
        }
        Json::Str(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Whether an observed value satisfies what a case wrote.
///
/// Case-insensitive, because `A1:C5` and `a1:c5` name the same range and
/// `TRUE` is what a spreadsheet person types. `nothing` and `none` are spelled
/// out for `null`: `assert fill_preview is nothing` is the sentence, and
/// `is null` reads like a bug report.
pub fn value_matches(observed: &str, expected: &str, negated: bool) -> bool {
    let o = observed.trim();
    let e = expected.trim();
    let same = o.eq_ignore_ascii_case(e)
        || (matches!(e.to_ascii_lowercase().as_str(), "null" | "nothing" | "none")
            && (o == "null" || o.is_empty()))
        || (e.eq_ignore_ascii_case("empty") && o.is_empty());
    same != negated
}

/// Whether the app is drawing, or is armed to draw, a fill preview.
///
/// Both keys, not one. A sweep that armed the fill handle shows in `filling`
/// with nothing previewed yet; one that got as far as a preview shows in
/// `fill_preview`. The regression this exists for went through the first and
/// out the second.
pub fn fill_preview_verdict(state: &Json) -> Result<(), String> {
    let filling = state.get("filling").and_then(Json::as_bool);
    let preview = state.get("fill_preview").map(scalar_text);
    let (Some(filling), Some(preview)) = (filling, preview) else {
        return Err(
            "the app reports no 'filling'/'fill_preview'; is the active tab a sheet?".to_string(),
        );
    };
    let previewing = preview != "null" && !preview.is_empty();
    if !filling && !previewing {
        return Ok(());
    }
    Err(format!(
        "expected: no fill armed and nothing previewed\n\
         observed: filling={filling}, fill_preview={preview}"
    ))
}

/// Which cells moved between a snapshot and now, said in full.
///
/// `before` is the authority for which cells are looked at: a snapshot is of a
/// named range, and a cell outside it changing is not this assertion's
/// business.
pub fn changed_cells(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (cell, was) in before {
        let now = after.get(cell).map(String::as_str).unwrap_or("");
        if now != was {
            out.push(format!(
                "{cell} was {} and is now {}",
                shown(was),
                shown(now)
            ));
        }
    }
    out
}

fn shown(s: &str) -> String {
    if s.is_empty() {
        "empty".to_string()
    } else {
        format!("'{s}'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_reads_as_a_test_would_write_it() {
        assert_eq!(scalar_text(&Json::Num(0.0)), "0");
        assert_eq!(scalar_text(&Json::Num(12.0)), "12");
        assert_eq!(scalar_text(&Json::Num(1.5)), "1.5");
        assert_eq!(scalar_text(&Json::Null), "null");
        assert_eq!(scalar_text(&Json::Bool(true)), "true");
        assert_eq!(scalar_text(&Json::Str("A1:C5".into())), "A1:C5");
    }

    #[test]
    fn a_value_matches_however_the_case_spelled_it() {
        assert!(value_matches("A1:C5", "a1:c5", false));
        assert!(value_matches("true", "TRUE", false));
        assert!(!value_matches("true", "false", false));
        assert!(value_matches("true", "false", true), "negation flips it");
        assert!(!value_matches("true", "true", true));
    }

    /// `null` has three spellings because all three turn up in a case, and a
    /// harness that only took one would send the author to the source.
    #[test]
    fn nothing_and_none_and_null_all_mean_the_same_absence() {
        for word in ["null", "nothing", "none", "NULL"] {
            assert!(value_matches("null", word, false), "{word}");
        }
        assert!(value_matches("", "empty", false));
        assert!(!value_matches("A1", "nothing", false));
    }

    #[test]
    fn a_fill_that_is_only_armed_still_fails_the_no_fill_expectation() {
        let armed = Json::obj(vec![
            ("filling", Json::Bool(true)),
            ("fill_preview", Json::Null),
        ]);
        let e = fill_preview_verdict(&armed).unwrap_err();
        assert!(e.contains("filling=true"), "{e}");
        // And one that got as far as a preview, with the flag already cleared.
        let previewed = Json::obj(vec![
            ("filling", Json::Bool(false)),
            ("fill_preview", Json::Str("A1:C5".into())),
        ]);
        let e = fill_preview_verdict(&previewed).unwrap_err();
        assert!(e.contains("A1:C5"), "{e}");
    }

    #[test]
    fn a_sheet_with_no_fill_anywhere_passes() {
        let clean = Json::obj(vec![
            ("filling", Json::Bool(false)),
            ("fill_preview", Json::Null),
        ]);
        assert!(fill_preview_verdict(&clean).is_ok());
    }

    /// A reply with neither key is a different problem from a fill being armed,
    /// and saying "no fill armed" would be a lie about a document tab.
    #[test]
    fn a_reply_without_the_keys_says_so_rather_than_passing() {
        let doc = Json::obj(vec![("tab", Json::Num(0.0))]);
        let e = fill_preview_verdict(&doc).unwrap_err();
        assert!(e.contains("is the active tab a sheet"), "{e}");
    }

    #[test]
    fn a_changed_cell_is_named_with_what_it_was_and_is() {
        let before: BTreeMap<String, String> = [("A1", "10"), ("B1", "20")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut after = before.clone();
        assert!(changed_cells(&before, &after).is_empty());
        after.insert("B1".to_string(), "10".to_string());
        let d = changed_cells(&before, &after);
        assert_eq!(d, vec!["B1 was '20' and is now '10'"]);
        // A cell that lost its value reads as empty, not as a missing key.
        after.insert("B1".to_string(), String::new());
        assert_eq!(
            changed_cells(&before, &after),
            vec!["B1 was '20' and is now empty"]
        );
    }

    /// Only the snapshot's own cells; the range is what was asserted about.
    #[test]
    fn a_cell_outside_the_snapshot_is_not_this_assertions_business() {
        let before: BTreeMap<String, String> =
            [("A1".to_string(), "10".to_string())].into_iter().collect();
        let after: BTreeMap<String, String> = [
            ("A1".to_string(), "10".to_string()),
            ("Z9".to_string(), "new".to_string()),
        ]
        .into_iter()
        .collect();
        assert!(changed_cells(&before, &after).is_empty());
    }

    #[test]
    fn a_canonicalized_windows_path_loses_its_extended_length_prefix() {
        assert_eq!(
            path_arg(Path::new(r"\\?\C:\x\basic.xlsx")),
            r"C:\x\basic.xlsx"
        );
        assert_eq!(path_arg(Path::new("/tmp/basic.xlsx")), "/tmp/basic.xlsx");
    }

    #[test]
    fn a_transcript_shows_every_step_and_ends_with_a_count() {
        let c = CaseOutcome {
            name: "drag does not fill".to_string(),
            steps: vec![
                StepOutcome {
                    line: 2,
                    source: "drag A1 -> C5".to_string(),
                    status: Status::Ok,
                    detail: String::new(),
                    evidence: None,
                },
                StepOutcome {
                    line: 3,
                    source: "assert no fill preview".to_string(),
                    status: Status::Failed,
                    detail: "observed: filling=true".to_string(),
                    evidence: Some(PathBuf::from("out.png")),
                },
            ],
        };
        let r = ScriptOutcome { cases: vec![c] };
        assert!(!r.passed());
        let text = r.report();
        assert!(text.contains("drag A1 -> C5"), "{text}");
        assert!(text.contains("FAIL  assert no fill preview"), "{text}");
        assert!(text.contains("observed: filling=true"), "{text}");
        assert!(text.contains("evidence: out.png"), "{text}");
        assert!(text.contains("1 case — 0 passed, 1 failed"), "{text}");
    }
}
