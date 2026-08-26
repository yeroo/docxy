# A scripted UI test harness for the suite

## Overview

Every visual change in the suite is currently verified by a person installing a
build and looking at it. That is why the last two regressions — a drag-to-select
that ran auto-fill, and dashes on an ordinary selection — reached an installer
before anyone noticed. Neither was reachable from a unit test: both live in
event handling and rendering, and gpui `#[test]` blows up the moment a view is
constructed.

This builds a harness that can **drive the app by verbs, screenshot it, and
assert on the pixels**, in an instance that cannot touch the real one.

```
test: drag-select does not fill
  launch --isolated --open fixtures/basic.xlsx
  drag A1 -> C5
  shot grid                       -> grid.png
  assert cells unchanged
  assert border A1:C5 solid       # not dashed
  assert no fill preview
```

### The isolation requirement comes first

A test instance must not touch the installed app's settings, workspace, or open
documents. Today the suite writes two things, both under
`dirs::config_dir()/docxy`:

- `session.json` — open tabs, files, theme (`session_path`, main.rs:189)
- `hot/` — one `.docx` sidecar per open Doc tab, rewritten on every persist
  (`hot_dir`, main.rs:3497)

Work is hot-persisted and restored *regardless of whether it was saved*, so a
test instance sharing that directory would overwrite the user's open documents.
This is the requirement the plan is built around, not a nicety.

⚠️ **The trap that makes isolation look complete when it is not.** There are two
different path-resolution mechanisms here:

| Path | Resolved by | Honours `APPDATA` env var? |
|---|---|---|
| `ctlcore::config_ctl_dir` | reads `APPDATA` directly (ctlcore/src/lib.rs:406) | yes |
| `dirs::config_dir()` | the Windows known-folder API | **no** |

So setting `APPDATA` for the child process isolates the control socket while
`session.json` and the hot sidecars still land in the real profile. Task 1
verifies this claim before anything is built on it, and the fix is a single
explicit override that BOTH paths go through.

### Driving by verbs, not synthetic input

`ctlcore` already exists and is how the terminal `docxy` is driven: a loopback
TCP listener on a random port, a minted token, and a discovery file naming both
(`ctlcore::serve`, lib.rs:150). `docxy/src/control.rs` is the working reference
implementation.

Using it means the harness never sends global synthetic input. That matters
beyond tidiness: an earlier session sent a synthetic drag that landed in the
user's terminal window because the target app was not in the foreground, and
gpui ignores `SendKeys` outright. Verbs have neither failure mode, and a test
can run without owning the desktop.

### Where the pixels come from

gpui has no window-to-image readback — `screen_capture_sources` (app.rs:1288)
is the screen-sharing source list, and `to_image_data` (platform.rs:2529) is
for SVG. So capture is harness-side, via Win32 `PrintWindow` against the test
window's HWND, which captures that window alone even when partly occluded.

The app supplies the *geometry*, the harness supplies the *pixels*: a control
verb returns the on-screen rect of a named region (`grid`, `chart-panel`, a
cell, a chart card), and the harness crops the window capture to it. Neither
side has to guess at the other's layout, and a region assertion never drifts
because a panel moved.

### Key benefits

- The two regressions just fixed become tests that fail if they return.
- A visual change can be checked without installing anything.
- Screenshots are saved as PNGs, so a human — or an agent that can read images
  — can look at what actually rendered rather than infer it.

## Context (from discovery)

### Files and components

- `ctlcore/` — `serve` (lib.rs:150), `config_ctl_dir` (:406), `instance_name`,
  `Request::arg`/`reply_ok`/`reply_err`, the `Json` type. Reused as-is.
- `docxy/src/control.rs` — the reference wiring for a ctlcore server in an app.
- `suite/docxy/src/main.rs` — `config_root` (:173), `session_path` (:189),
  `hot_dir` (:3497), the CLI file arguments (`std::env::args_os`, ~:18885), and
  the event handlers the verbs will drive.
- `suite/docxy/Cargo.toml` — the `suite` binary; `ctlcore` is not yet a
  dependency of it.

### What must NOT change

- The suite's normal startup. A build with no harness flag must behave exactly
  as it does today, use the real config directory, and start no listener. The
  harness is opt-in per process, never a mode the user can end up in by
  accident.

### Dependencies

None outside the repo. `ctlcore` is already in the root workspace; the suite is
a separate workspace and will need a path dependency on it. Both workspaces
build clean; `gridcore` has 370 tests and the suite crate 124.

## Development Approach

- **Testing approach**: Regular — code first, tests in the same task.
- **CRITICAL: every task MUST include new/updated tests** for code changes in
  that task; tests are a required part of the checklist, covering success and
  error cases.
- **CRITICAL: all tests must pass before starting the next task.**
- **CRITICAL: update this plan file when scope changes during implementation.**

⚠️ Much of this is I/O and rendering. Keep the decisions in **pure free
functions** — parsing a test script, resolving a region name to a rect given a
layout, deciding whether a sampled row of pixels is dashed or solid, comparing
an expectation to an observation. Those are what unit tests cover. The harness
itself is exercised by running it.

### Build and test commands

Two separate cargo workspaces; a green `suite/` says nothing about the root one.

```bash
cargo build  --manifest-path suite/Cargo.toml
cargo test   --manifest-path suite/Cargo.toml
cargo build --all-targets
cargo test  -p gridcore
cargo clippy -p gridcore --all-targets -- -D warnings
cargo fmt --check
```

## Implementation Steps

### Task 1: Prove isolation, before anything can rely on it

- [x] verify the claim in Context: does `dirs::config_dir()` on Windows read the
      `APPDATA` environment variable, or the known-folder API? Write a tiny
      program that sets `APPDATA` and prints both `dirs::config_dir()` and
      `ctlcore::config_ctl_dir("docxy")`, and record the result in this plan
- [x] ⚠️ if they disagree — the expectation — an `APPDATA` override alone is NOT
      isolation, and every later task depends on knowing that. If they agree,
      say so here and simplify Task 2 accordingly
- [x] add a single `config_root()` helper in the suite that returns the override
      when `DOCXY_CONFIG_DIR` is set and `dirs::config_dir()` otherwise, and
      route `session_path()` and `hot_dir()` through it
- [x] audit for any other write path — recent files, logs, caches, crash dumps,
      temp sidecars — and route or document each. Grep for `dirs::`, `AppData`,
      `temp_dir`, `File::create`, `write` in the suite and list what was found
      in this plan, so "we checked" is a record rather than a claim
- [x] write tests for `config_root()`: override set, override unset, override
      set to a relative path, and that `session_path`/`hot_dir` both sit under
      whatever it returns
- [x] run tests in both workspaces — must pass before Task 2

#### Result: measured, and they disagree

A throwaway crate depending on `dirs = "5"` and path-depending on `ctlcore`
printed both paths — once with the real environment, then twice with `APPDATA`
pointed at `target\harness-appdata`: set in-process via `set_var`, and again as
the environment of a child process, since that is how the harness would do it.
Windows 11, `dirs` 5.x. Identical results both ways:

```
APPDATA env                      = ...\docxy\target\harness-appdata
dirs::config_dir()               = C:\Users\boris\AppData\Roaming        <-- IGNORED it
ctlcore::config_ctl_dir("docxy") = ...\target\harness-appdata\docxy\ctl  <-- followed it
```

**They disagree, as expected.** `dirs::config_dir()` goes to the known-folder
API and never reads `APPDATA`; `ctlcore::config_ctl_dir` reads the variable
directly. So an `APPDATA` override moves the control socket into the sandbox
while `session.json` and every hot sidecar keep landing in the user's real
profile — precisely the "looks isolated, isn't" failure the Overview warns of.

Task 2 is therefore **not** simplified. Two consequences it must honour:

1. `--harness` must *require* `DOCXY_CONFIG_DIR`; an `APPDATA` override alone
   must not be accepted as isolation.
2. The discovery file must be written under `config_root()` explicitly, rather
   than left to `ctlcore::config_ctl_dir`'s own `APPDATA` lookup — otherwise the
   socket and the session state can end up in two different sandboxes.

#### The write-path audit

Greps over `suite/` (the crate is one file, `suite/docxy/src/main.rs`, plus
`build.rs`) for `dirs::`, `AppData`/`APPDATA`, `temp_dir`/`tempfile`,
`File::create`, `fs::write`, `fs::create_dir`, `fs::remove`, `fs::copy`,
`fs::rename`, `OpenOptions`, `current_dir`, `home_dir`, `data_dir`, `cache_dir`
and `data_local_dir`. Everything found:

| Site | What it writes | Disposition |
|---|---|---|
| `session_path()` (main.rs:189) | `session.json` | **routed** through `config_root()` |
| `hot_dir()` (main.rs:3497) | the `hot/` directory | **routed** through `config_root()` |
| `persist()` (:3722, :3728, :3755) | `hot/tab-N.docx`, `hot/tab-N.xlsx`, `session.json` | **routed** — all three derive from the two helpers above |
| `persist()` (:3710, :3753) | `create_dir_all` for those two directories | **routed** — same derivation |
| `save_doc` (:8973) | the document the user chose | **documented, not routed** — an explicit save to an explicit path; a harness test reaches it only by issuing a save verb at a path it picked itself |
| `save_sheet` (:9020) | the workbook the user chose (`rfd` Save-As when new) | **documented, not routed** — same |
| `save_doc` fallback (:8969) | `current_dir()/<title>` for a never-saved doc | **documented** — the harness gives the child its own working directory, so this stays inside the run too |

Nothing else writes. There are **no** logs, caches, crash dumps, recent-file
lists, temp files, or `data_dir`/`cache_dir` uses in the suite: the only
`std::env::var` in the crate is `USERNAME` (:6114, read-only, for the comment
author name), and the only two `dirs::` calls are the ones now behind
`config_root()`. `build.rs` writes only into `OUT_DIR`.

### Task 2: An opt-in harness mode that starts a control server

- [ ] add `ctlcore` as a path dependency of the suite crate
- [ ] add a `--harness` flag (or `DOCXY_HARNESS=1`): with it the suite starts a
      `ctlcore::serve` listener and writes its discovery file under
      `config_root()`; without it, nothing changes and no listener is started
- [ ] follow `docxy/src/control.rs` for the wiring — request pump, token check,
      `reply_ok`/`reply_err` shapes — rather than inventing a second style
- [ ] make the flag imply the isolated config root is REQUIRED: refuse to start
      the harness against the real config directory, so a mistyped invocation
      cannot drive the user's own instance
- [ ] write tests for the pure parts: flag parsing, and the refusal when the
      config root is not an override
- [ ] run tests — must pass before Task 3

### Task 3: Verbs that drive the grid and the panel

- [ ] implement the verbs a UI test needs, each replying with enough state to
      assert on: `open` (a fixture path), `click-cell`, `drag` (from → to),
      `type`, `key`, `select-chart`, `focus-field`, `cell` (read a cell's value),
      `selection` (read the current selection), `quit`
- [ ] route each verb through the SAME entry point the real UI uses, not a
      parallel path — a verb that bypasses the handler under test would pass
      while the app is broken, which is the one way this harness could be worse
      than useless
- [ ] write tests for verb parsing and argument validation, including a verb
      naming a cell that does not exist and a drag with a malformed range
- [ ] run tests — must pass before Task 4

### Task 4: The app reports geometry; the harness takes pixels

- [ ] add a `rect` verb: given a region name (`window`, `grid`, `chart-panel`,
      `cell:A1`, `chart:1`), reply with its on-screen rectangle in physical
      pixels, or an error naming the unknown region
- [ ] write the harness-side capture: find the test instance's HWND from its
      PID, `PrintWindow` it to a bitmap, save as PNG, and crop to a rect
- [ ] make every capture land in the run's own output directory alongside the
      test that took it, so a failing test's evidence is findable
- [ ] write tests for the pure parts: region-name parsing, and cropping a rect
      to an image's bounds (including a rect partly outside the image, which
      must clamp rather than panic)
- [ ] run tests — must pass before Task 5

### Task 5: Assertions about what was drawn

- [ ] implement the pixel probes the first tests need: sample a straight run of
      pixels along an edge and decide **solid vs dashed vs absent**, and match a
      colour within a tolerance so anti-aliasing does not fail a true result
- [ ] express expectations in the test's own vocabulary — `border A1:C5 solid`,
      `border A1:D5 dashed teal` — resolving to a rect via Task 4 and a probe
      via this task
- [ ] on failure, report what was expected, what was observed, and the path to
      the PNG. A failure that only says "assertion failed" would send the reader
      back to installing a build, which is what this plan exists to avoid
- [ ] write tests for the probes over synthetic images: a solid line, a dashed
      line at the shader's `2W` dash / `1W` gap pitch, an empty edge, and an
      anti-aliased edge that must still read as its nominal colour
- [ ] run tests — must pass before Task 6

### Task 6: The script format, and the first real cases

- [ ] define the test script format and write its parser as a pure function
- [ ] write the two regression cases from the Overview: a drag-to-select that
      must not fill and must not dash, and a pointed range that must dash
- [ ] add a case for one selection at a time: with a cell being edited, select a
      chart, and assert the keyboard goes to the chart — the invisible-caret bug
      that swallowed every keystroke
- [ ] add a fixture workbook under the harness's own directory; it must not read
      anything from the user's documents
- [ ] write tests for the script parser: a valid script, an unknown verb, a
      malformed assertion, and a script that references an undefined region
- [ ] run the harness end to end and record in this plan what it actually
      caught, including any case that could not be expressed
- [ ] run tests — must pass before Task 7

### Task 7: Verify acceptance criteria

- [ ] verify a harness run cannot touch the real config: run it, then confirm
      the real `session.json` and `hot/` are byte-identical to before
- [ ] verify the suite with no harness flag is unchanged — real config root, no
      listener, no discovery file
- [ ] verify each test case from Task 6 fails when its fix is reverted, which is
      the only proof a test tests anything
- [ ] run all tests in both workspaces, clippy and fmt clean

### Task 8: [Final] Document it

- [ ] document how to write and run a harness test, the verbs and regions
      available, and the isolation guarantee and how it is enforced
- [ ] record the gpui-has-no-readback finding so the next person does not go
      looking for a screenshot API

*Note: ralphex automatically moves completed plans to `docs/plans/completed/`*

## Technical Details

### Why the app reports rects instead of the harness guessing

Region geometry lives in the layout, which only the app knows. A harness that
computed "the grid starts 120px down" would break on a ribbon change and would
have to be corrected for DPI. Asking the app keeps one source of truth and
makes `cell:A1` a legitimate region even though its position depends on scroll,
row heights and frozen panes.

### Why not golden images

Considered and rejected: reference PNGs diffed wholesale catch every unintended
change, but they are brittle across GPU, DPI and font differences, and the
practical failure mode is that a diff gets blanket-approved rather than
investigated. Targeted probes state what is being asserted, so a failure names
a property rather than a pixel count. The full PNG is saved regardless, so
nothing stops a human looking at the whole picture.

## Post-Completion

**Manual verification**

- Run the harness while the real suite is open, then confirm the real instance's
  tabs, documents and theme are untouched.
- Deliberately break one of the fixed regressions, run the harness, and confirm
  the right test fails with a useful message and a PNG.

**Deferred**

- Running this in CI. It needs a desktop session for `PrintWindow`, so it is a
  local-and-on-demand tool until that is solved.
- Any verb for the Doc or Mail tabs; this plan covers the sheet UI, which is
  where the regressions have been.
