//! `uiharness` — drive a `--harness` suite instance from the command line.
//!
//! Enough of a front end to send a verb, read a region's rectangle and take a
//! picture, which is what Task 4 of the harness plan needs and what the script
//! runner (Task 6) will build on.
//!
//! ```text
//! uiharness --config <sandbox> ping
//! uiharness --config <sandbox> call drag '{"from":"A1","to":"C5"}'
//! uiharness --config <sandbox> rect cell:A1:C5
//! uiharness --config <sandbox> shot grid --run out/runs/1 --test drag-select
//! uiharness --config <sandbox> assert border A1:C5 solid
//! uiharness run cases/drag-select.uit
//! ```
//!
//! `run` is the one command that does not need an instance already up: it
//! starts its own in a throwaway config root, runs the script against it, and
//! stops it again. Every other command attaches to one that is already there,
//! which is what makes them useful for working a case out by hand before
//! writing it down.
//!
//! `--config` is the sandbox the instance was started with
//! (`DOCXY_CONFIG_DIR`); the control directory is derived from it exactly as
//! the app derives it, so the two cannot end up looking in different places.
//! It defaults to `DOCXY_CONFIG_DIR` in this process's own environment.

use ctlcore::json::Json;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use uiharness::probe::ProbeOpts;
use uiharness::{Driver, Run, check_border, control_dir, parse_border};

const USAGE: &str = "\
uiharness — drive a suite instance started with --harness

usage:
  uiharness [--config DIR | --ctl DIR] [--instance NAME] <command>

commands:
  run SCRIPT...                 launch a sandboxed instance and run a test script
  ping                          check the connection and print the instance
  call VERB [JSON]              send any verb, print its reply
  rect REGION                   print a region's desktop rectangle
  shot REGION                   capture a region to a PNG
  window                        capture the whole window to a PNG
  assert EXPECTATION...         capture a region and check what was drawn

expectations:
  border A1:C5 solid            every edge of the range, unbroken
  border A1:D5 dashed teal      every edge dashed, in the brand teal
  border A1 top solid teal      one named edge
  no border B7                  nothing along any edge

options for shot/window:
  --run DIR                     the run's output directory (default: ./uiharness-runs)
  --test NAME                   the test the capture belongs to (default: adhoc)
  --out FILE                    write here instead of under the run directory

options for run:
  --suite EXE                   the built suite to drive (default: the newest
                                target/{release,debug} build; $UIHARNESS_SUITE)
  --sandbox DIR                 the throwaway config root (default: <run>/sandbox)
  --keep                        leave the instance running after the script ends

regions:
  window  grid  chart-panel  cell:B3  cell:A1:C5  chart:0
";

fn main() -> ExitCode {
    match run() {
        Ok(out) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("uiharness: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The command line, as parsed.
struct Args {
    config: Option<PathBuf>,
    ctl: Option<PathBuf>,
    instance: Option<String>,
    run_dir: PathBuf,
    test: String,
    out: Option<PathBuf>,
    suite: Option<PathBuf>,
    sandbox: Option<PathBuf>,
    keep: bool,
    rest: Vec<String>,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        config: std::env::var_os("DOCXY_CONFIG_DIR").map(PathBuf::from),
        ctl: None,
        instance: None,
        run_dir: PathBuf::from("uiharness-runs"),
        test: "adhoc".to_string(),
        out: None,
        suite: None,
        sandbox: None,
        keep: false,
        rest: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| -> Result<String, String> {
            it.next()
                .ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"))
        };
        match arg.as_str() {
            "--config" => a.config = Some(PathBuf::from(value("--config")?)),
            "--ctl" => a.ctl = Some(PathBuf::from(value("--ctl")?)),
            "--instance" => a.instance = Some(value("--instance")?),
            "--run" => a.run_dir = PathBuf::from(value("--run")?),
            "--test" => a.test = value("--test")?,
            "--out" => a.out = Some(PathBuf::from(value("--out")?)),
            "--suite" => a.suite = Some(PathBuf::from(value("--suite")?)),
            "--sandbox" => a.sandbox = Some(PathBuf::from(value("--sandbox")?)),
            "--keep" => a.keep = true,
            "-h" | "--help" => return Err(USAGE.to_string()),
            other if other.starts_with("--") => {
                return Err(format!("unknown option '{other}'\n\n{USAGE}"));
            }
            other => a.rest.push(other.to_string()),
        }
    }
    Ok(a)
}

fn run() -> Result<String, String> {
    let a = parse()?;
    // `run` starts its own instance, so it is answered before the code that
    // insists on being told where an existing one is.
    if a.rest.first().map(String::as_str) == Some("run") {
        return run_scripts(&a);
    }
    let ctl = match (&a.ctl, &a.config) {
        (Some(d), _) => d.clone(),
        (None, Some(c)) => control_dir(c),
        (None, None) => {
            return Err(format!(
                "no sandbox given: pass --config <dir> (the DOCXY_CONFIG_DIR the \
                 instance was started with) or --ctl <dir>\n\n{USAGE}"
            ));
        }
    };
    let cmd = a.rest.first().map(String::as_str).unwrap_or("");
    if cmd.is_empty() {
        return Err(USAGE.to_string());
    }
    let d = Driver::connect(&ctl, a.instance.as_deref())?;

    match cmd {
        "ping" => Ok(d.call("ping", Json::Obj(Vec::new()))?.to_string()),

        "call" => {
            let verb = a
                .rest
                .get(1)
                .ok_or_else(|| format!("call needs a verb\n\n{USAGE}"))?;
            let args = match a.rest.get(2) {
                Some(text) => {
                    Json::parse(text).map_err(|e| format!("'{text}' is not JSON: {e}"))?
                }
                None => Json::Obj(Vec::new()),
            };
            Ok(d.call(verb, args)?.to_string())
        }

        "rect" => {
            let region = a
                .rest
                .get(1)
                .ok_or_else(|| format!("rect needs a region\n\n{USAGE}"))?;
            let r = d.settle(region)?;
            Ok(format!(
                "{region}: {}x{} at ({},{})  scale {}  frame {}",
                r.rect.w, r.rect.h, r.rect.x, r.rect.y, r.scale, r.frame
            ))
        }

        "shot" | "window" => {
            let region = if cmd == "window" {
                "window".to_string()
            } else {
                a.rest
                    .get(1)
                    .ok_or_else(|| format!("shot needs a region\n\n{USAGE}"))?
                    .clone()
            };
            let (img, cap) = d.shot(&region)?;
            let path = save(&a, &region, &img)?;
            Ok(format!(
                "{region}: {}x{} -> {}  (via {})",
                img.w,
                img.h,
                path.display(),
                cap.how
            ))
        }

        // Take the picture and read it. The expectation names the region, so
        // the crop that is checked and the PNG that is filed are the same
        // pixels, from the same settled frame.
        "assert" => {
            let text = a.rest[1..].join(" ");
            let exp = parse_border(&text)?;
            let (img, cap) = d.shot(&exp.region)?;
            let path = save(&a, &exp.region, &img)?;
            let check = check_border(&exp, &img, ProbeOpts::default());
            let mut report = check.report(Some(&path));
            report.push_str(&format!(
                "  capture:  {}x{} via {}\n",
                img.w, img.h, cap.how
            ));
            // A failed expectation is a failed process: `Err` here is what puts
            // the report on stderr and a non-zero code on the exit.
            if check.passed() {
                Ok(report)
            } else {
                Err(report)
            }
        }

        other => Err(format!("unknown command '{other}'\n\n{USAGE}")),
    }
}

/// Run one or more script files against an instance this process starts and
/// stops.
///
/// The script is parsed — all of them, in fact — before the app is launched, so
/// a typo costs milliseconds rather than a cold start. That ordering is the
/// point of [`uiharness::script`] being pure.
fn run_scripts(a: &Args) -> Result<String, String> {
    let paths = &a.rest[1..];
    if paths.is_empty() {
        return Err(format!("run needs a script file\n\n{USAGE}"));
    }
    let mut scripts = Vec::new();
    for p in paths {
        let path = PathBuf::from(p);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let script =
            uiharness::parse_script(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let base = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        scripts.push((path, base, script));
    }

    let exe = uiharness::launch::find_suite(a.suite.as_deref())?;
    let run = Run::create(&a.run_dir).map_err(|e| format!("{}: {e}", a.run_dir.display()))?;
    // ⚠️ The default sandbox is a fixed path under a fixed run directory, so it
    // is the SAME directory on every invocation — and the app persists into it:
    // `quit` writes `session.json` plus a hot sidecar holding each tab's live
    // content, and the next launch restores those in preference to the file on
    // disk. Left alone, run N+1 would start with run N's tabs and their unsaved
    // edits, which is exactly the "testing that instance's history" the harness
    // starts its own instance to avoid. A sandbox the caller named is theirs to
    // manage (that is what `--sandbox` is for: keeping one across runs).
    let sandbox = match &a.sandbox {
        Some(p) => p.clone(),
        None => {
            let p = run.dir().join("sandbox");
            if let Err(e) = std::fs::remove_dir_all(&p) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(format!("clearing the sandbox {}: {e}", p.display()));
                }
            }
            p
        }
    };
    let mut app = uiharness::launch::launch(&exe, &sandbox)?;
    let ctl = app.ctl_dir();

    let driver = match Driver::connect(&ctl, a.instance.as_deref()) {
        Ok(d) => d,
        // A refusal from the isolation gate exits before it ever publishes a
        // socket, so the connect timing out is the symptom and the exit is the
        // cause. Say both.
        Err(e) => {
            return Err(match app.exited() {
                Some(why) => format!("{e}\n{why}"),
                None => e,
            });
        }
    };

    let mut out = String::new();
    let mut all_passed = true;
    for (path, base, script) in &scripts {
        out.push_str(&format!("{}\n", path.display()));
        let outcome = uiharness::Runner::new(&driver, &run, base).run_script(script);
        all_passed &= outcome.passed();
        out.push_str(&outcome.report());
    }
    out.push_str(&format!("evidence: {}\n", run.dir().display()));
    out.push_str(&format!("sandbox:  {}\n", sandbox.display()));

    if a.keep {
        out.push_str(&format!(
            "the instance is still running (pid {}); --keep was given\n",
            driver.pid()
        ));
        app.detach();
    } else {
        app.shutdown(Some(&driver));
    }

    if all_passed { Ok(out) } else { Err(out) }
}

/// File a capture: `--out` if the caller named a file, otherwise under the
/// run's own directory in the folder named for the test that took it.
fn save(a: &Args, region: &str, img: &uiharness::Image) -> Result<PathBuf, String> {
    match &a.out {
        Some(p) => {
            uiharness::png::write(p, img).map_err(|e| format!("{}: {e}", p.display()))?;
            Ok(p.clone())
        }
        None => {
            let run =
                Run::create(&a.run_dir).map_err(|e| format!("{}: {e}", a.run_dir.display()))?;
            run.save(&a.test, region, img)
                .map_err(|e| format!("saving the capture: {e}"))
        }
    }
}
