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
//! ```
//!
//! `--config` is the sandbox the instance was started with
//! (`DOCXY_CONFIG_DIR`); the control directory is derived from it exactly as
//! the app derives it, so the two cannot end up looking in different places.
//! It defaults to `DOCXY_CONFIG_DIR` in this process's own environment.

use ctlcore::json::Json;
use std::path::PathBuf;
use std::process::ExitCode;
use uiharness::probe::ProbeOpts;
use uiharness::{Driver, Run, check_border, control_dir, parse_border};

const USAGE: &str = "\
uiharness — drive a suite instance started with --harness

usage:
  uiharness [--config DIR | --ctl DIR] [--instance NAME] <command>

commands:
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
