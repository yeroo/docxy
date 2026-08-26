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
- `uiharness/` — added in Task 4, in the ROOT workspace: the driver side (the
  `ctlcore` client, `PrintWindow` capture, a from-scratch PNG encoder, the run
  output directory). Depends on `ctlcore` and `opccore` only.

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
cargo test  -p uiharness          # the harness driver (Task 4 on)
cargo clippy -p gridcore --all-targets -- -D warnings
cargo clippy -p uiharness --all-targets -- -D warnings
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

- [x] add `ctlcore` as a path dependency of the suite crate
- [x] add a `--harness` flag (or `DOCXY_HARNESS=1`): with it the suite starts a
      `ctlcore::serve` listener and writes its discovery file under
      `config_root()`; without it, nothing changes and no listener is started
- [x] follow `docxy/src/control.rs` for the wiring — request pump, token check,
      `reply_ok`/`reply_err` shapes — rather than inventing a second style
- [x] make the flag imply the isolated config root is REQUIRED: refuse to start
      the harness against the real config directory, so a mistyped invocation
      cannot drive the user's own instance
- [x] write tests for the pure parts: flag parsing, and the refusal when the
      config root is not an override
- [x] run tests — must pass before Task 3

#### What was built

`suite/docxy/src/harness.rs` — the whole surface, so `main.rs` gains only the
call sites. The decisions are free functions with tests: `parse_args`,
`env_flag`, `gate`, `same_dir`, `control_dir`.

- **Enabling it.** `--harness` or `DOCXY_HARNESS=1`. `parse_args` keeps
  unrecognized `--` arguments instead of dropping them, because the old
  `filter(is_file)` would have swallowed a mistyped `--harnes` and the app would
  have come up looking normal while the driver waited forever for a socket.
- **The gate.** `gate(DOCXY_CONFIG_DIR, dirs::config_dir())` returns the sandbox
  root, or refuses: no override, a blank override, or an override naming the
  real config directory (compared textually, ignoring separator style, a
  trailing slash and — on Windows — case; `canonicalize` is wrong here since the
  sandbox usually does not exist yet, and it would fail open). Refusal exits 2
  rather than starting degraded: a harness up without its socket hangs its
  driver.
- **Where the socket is published.** `<config_root>/suite/ctl`, derived from the
  gated root — *not* `ctlcore::config_ctl_dir`, which does its own `APPDATA`
  lookup and could put the socket in a different sandbox from `session.json`
  (the Task 1 trap).
- **The pump.** The one place this could not follow `docxy/src/control.rs`: the
  terminal editor owns its event loop and selects over requests, while here the
  thread belongs to gpui. So `attach` spawns a `window.spawn` foreground task
  that `try_recv`s and applies each request through `update_in` — on the app's
  own thread, no locking — sleeping 8ms when idle. The `Server` and the `Task`
  are parked on `Docxy::harness`, so the discovery file is removed and the pump
  cancelled when the window goes. Token checking and the `reply_ok`/`reply_err`
  shapes are ctlcore's, unchanged.
- **The verb table** has only `ping` (instance, pid, config_root, tab count) —
  Task 3 fills it in. It is enough to prove the reply is produced on the app
  thread, since it reports the app's own state.

Verified against the real built `suite.exe`, not just in unit tests:

| Run | Result |
|---|---|
| `--harness` + sandbox `DOCXY_CONFIG_DIR` | discovery file written under the sandbox; `ping` → `{"ok":true,…,"tabs":1}`; `nope` → `unknown verb 'nope'`; wrong token → `unauthorized`; the real `%APPDATA%\docxy` byte-identical before/after; `session.json` + `hot/tab-0.docx` landed in the sandbox |
| `--harness`, no `DOCXY_CONFIG_DIR` | exit 2, message naming the variable |
| `--harness`, `DOCXY_CONFIG_DIR=%APPDATA%` | exit 2, "will not drive the installed app's own state" |
| no flag, sandbox root | app runs; `suite/ctl` never created — nothing listens |

19 new unit tests; `cargo test --manifest-path suite/Cargo.toml` 143 passed,
clippy `-D warnings` and `cargo fmt --check` clean, root workspace `ctlcore`
30 passed.

### Task 3: Verbs that drive the grid and the panel

- [x] implement the verbs a UI test needs, each replying with enough state to
      assert on: `open` (a fixture path), `click-cell`, `drag` (from → to),
      `type`, `key`, `select-chart`, `focus-field`, `cell` (read a cell's value),
      `selection` (read the current selection), `quit`
- [x] route each verb through the SAME entry point the real UI uses, not a
      parallel path — a verb that bypasses the handler under test would pass
      while the app is broken, which is the one way this harness could be worse
      than useless
- [x] write tests for verb parsing and argument validation, including a verb
      naming a cell that does not exist and a drag with a malformed range
- [x] run tests — must pass before Task 4

#### The verbs, and the entry point each one drives

| Verb | Args | Goes through |
|---|---|---|
| `ping` | — | (the channel itself) |
| `open` | `path` | `open_args` — the command-line path |
| `click-cell` | `cell`, `shift?`, `double?` | `grid_press_cell` → `cell_click` → `grid_release` |
| `drag` | `from`+`to`, or `range` | `grid_press_cell` → `grid_drag_over` per cell crossed → `grid_release` |
| `type` | `text` | `on_key`, one event per character |
| `key` | `key`, or `keys: []` | `on_key` |
| `select-chart` | `index` | `chart_press` → `grid_release` |
| `focus-field` | `field` | `ref_field_focus` |
| `cell` | `cell` | reads `cell_text` / `edit_string` |
| `selection` | — | reads |
| `quit` | — | `persist`, then the app stops |

Four handler bodies were **lifted out of their closures into methods** so a verb
calls the code the pointer calls rather than a second copy of the same rules:
`grid_press_cell` (out of `grid_press`, past the pixel hit-test), `cell_click`
(the cell's own `on_click`), `grid_drag_over` (the cell's `on_mouse_move` —
which is where "is this sweep a fill or a selection?" is decided, i.e. the
regression itself), and `ref_field_focus` (a reference field's mouse-down). The
render sites now call those methods, so there is one body each.

Every driving verb replies with the same state object — `sel`, `anchor`,
`range`, `editing`/`edit`, `dirty`, `chart_sel`, `panel_chart`, `charts`,
`field`/`field_text`, and the four the regressions are about: `filling`,
`fill_preview`, `picking`, `sel_hidden`. So `drag A1:C5` already answers "did it
select without filling" without a screenshot; Task 5 adds the pixels for
"solid, not dashed".

Verified against the built `suite.exe` driven over the socket, not only in unit
tests: `open`, `click-cell` (incl. double-click opening the editor), `drag`
(A1→C5 selected `A1:C5` with `filling:false`, `fill_preview:null` and A1/A2
unchanged), `type`+`enter` (committed 42 into C5, tab went `dirty`), `key` with
a list (`down,down,shift+right` → `A4:B4`), `selection`, `cell`, and `quit`
(replied, then the process went and its discovery file with it). Refusals
checked live too: `A0`, `nonsense`, `A1:`, a missing `from`, a missing `cell`,
`banana` as a key, empty `text`, an out-of-range chart, an unknown field, an
unknown verb, a missing file.

**Two scope notes, so they are a record rather than a surprise in Task 6.**

1. `select-chart` and the Chart-panel `focus-field`s have no *success* path to
   drive yet: no fixture in the repo carries a chart, and a sheet's Insert ▸
   Chart is a mouse-only ribbon button (the KeyTip ribbon is the document one),
   so nothing reachable by verb can author one. Their refusals are exercised;
   the success paths need the chart-bearing fixture Task 6 adds.
2. `focus-field` on an entry-bar field (`cond-format`, `validation`, `sort`,
   `text-to-columns`) refuses unless that bar is already open, because a field
   focused while its bar is shut is a state the UI cannot reach. Opening one
   needs a ribbon-command verb, which this plan does not have; Task 6 will say
   so if a case wants it.

22 new unit tests (165 total in the suite crate), clippy `-D warnings` and
`cargo fmt --check` clean in both workspaces; root workspace `gridcore` 370 and
`ctlcore` 30 still pass.

### Task 4: The app reports geometry; the harness takes pixels

- [x] add a `rect` verb: given a region name (`window`, `grid`, `chart-panel`,
      `cell:A1`, `chart:1`), reply with its on-screen rectangle in physical
      pixels, or an error naming the unknown region
- [x] write the harness-side capture: find the test instance's HWND from its
      PID, `PrintWindow` it to a bitmap, save as PNG, and crop to a rect
- [x] make every capture land in the run's own output directory alongside the
      test that took it, so a failing test's evidence is findable
- [x] write tests for the pure parts: region-name parsing, and cropping a rect
      to an image's bounds (including a rect partly outside the image, which
      must clamp rather than panic)
- [x] run tests — must pass before Task 5

#### The regions, and where each answer comes from

| Region | Answered from |
|---|---|
| `window` | `window.viewport_size()` — the client area |
| `grid` | `ListState::viewport_bounds()`, the row list's own measured band |
| `cell:B3`, `cell:A1:C5` | `bounds_for_item` per row + `col_span_x` per column |
| `chart-panel` | a `probe` element on the panel |
| `chart:N` | a `probe` element on the card |

`cell:` takes a range as readily as a single cell, so Task 5's `border A1:C5
solid` names the selection rather than its two corners. That is beyond the
plan's list; `cells:` is a synonym that reads better for one.

**Nothing here recomputes a position the renderer already decided.** Rows come
from the list's per-item bounds and columns from `col_span_x` — a new pure
function that is the exact inverse of the existing `col_at_x` hit-test, walking
gutter, then frozen columns, then the scrolled window, the same way.
`col_span_x_inverts_col_at_x` pins that down over uneven widths and four
freeze/scroll combinations: every x the hit-test resolves to a column falls
inside that column's reported span. A crop one column off would assert on the
neighbour's pixels and still pass.

The Chart panel and the chart cards could not work that way — their geometry is
decided by the layout engine, and a card's position folds in the scroll offset,
hidden rows, frozen panes and a drag in progress. So they carry a **probe**: a
zero-paint `canvas`, positioned `absolute` and inset to zero, which records
where it was laid out. `Probes` keeps two lists and `render` moves `next` into
`last` as each frame starts, so `rect` always answers from a frame that
finished. (The same pattern as the existing `ruler_x0` cell, which a canvas
already writes each paint.)

Refusals name the thing: an unknown region lists the ones there are, `cell:A0`
is not a cell, `chart:last` is not an index, a hidden row says so, and a column
scrolled off says which side it went off. A cell rectangle is refused outright
on a sheet with frozen ROWS — they render outside the list, so the list cannot
locate them, which is the same limit `cell_at` has; guessing would put a crop
over the wrong band.

#### The staleness trap, and the `frame` verb

Found by running it, not by reading it. Opening a chart workbook and asking for
`chart:0` straight afterwards answered **"scrolled out of the grid's view"**
while the chart was plainly on screen. Two lags compose:

1. A verb only marks the view dirty. When its reply goes out, the frame that
   shows what it did may not have been laid out.
2. A probe records during the layout of its own frame, so the newest *complete*
   set is always one frame behind.

So `rect` alone cannot be trusted straight after a verb — and the failure mode
is the bad one: it silently reports the picture from before the change.

The fix is a `frame` verb, which returns the frame count and asks for a repaint.
`Driver::settle` reads it as `f0` and waits for **`f0 + 2`** before measuring.
Not `f0 + 1` — that frame may have begun before the verb landed. At `f0 + 2` the
frame in between began after the read, hence after the verb, and its probes are
what `rect` now answers from. `shot` settles once and then both measures and
photographs, so the rect and the pixels come from the same state.

#### The harness side: a new `uiharness` crate

In the ROOT workspace (`uiharness/`), not the suite one: it needs Win32 and
nothing from gpui, and the suite workspace stays as lean as it is deliberately.

- `image.rs` — an RGBA buffer, `clamp_rect`, `crop`. A rect partly outside the
  image clamps (a chart half out of view still has pixels worth looking at);
  wholly outside is an `Err` naming both rectangles. Edges are computed in
  `i64`, so `i32::MIN` plus `u32::MAX` cannot wrap into a bogus rectangle.
- `deflate.rs` + `png.rs` — a real PNG with **no image or compression crate**:
  fixed-Huffman DEFLATE with greedy LZ77 over a hash chain, the zlib wrapper,
  PNG chunks with `opccore::zipwrite::crc32`, and per-row filtering
  (None/Sub/Up/Paeth by the usual heuristic). Hand-rolling this is only
  reasonable because the repo already owns an inflater: every deflate test
  round-trips through `opccore::inflate::inflate_raw`, including **every one of
  the 29 length and 30 distance codes**, and the PNG tests decode the file back
  with a reader written straight from RFC 2083. A 1180x800 window capture comes
  out at 68 KB from 3.7 MB of pixels.
- `capture.rs` — `EnumWindows` to the visible top-level window of the PID (owned
  windows skipped, so a tooltip is not mistaken for the app), then `PrintWindow`
  with `PW_RENDERFULLCONTENT` into a top-down 32-bit DIB.
  `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` first — without it
  `GetWindowRect` reports a 96-DPI fiction and every crop on a scaled display is
  off by that ratio.
- `driver.rs` — `Driver::connect` / `call` / `rect` / `frame` / `settle` /
  `shot`, over `ctlcore::client`. `control_dir` is derived from the sandbox root
  exactly as the app derives it, so the two cannot look in different places.
- `run.rs` — `<run>/<test>/<region>.png`, via a `slug` that strips what a
  Windows path would swallow. `cell:A1:C5` must not keep its colons: on Windows
  that names an alternate data stream, so the PNG would vanish rather than fail.
- `main.rs` — `uiharness --config <sandbox> {ping|call|rect|shot|window}`.

**One addition the plan did not call for, and why.** `PrintWindow` only reaches
DWM-redirected content, and gpui draws with DirectX, so a GPU-composed window
can come back blank. `capture_window` detects that (`is_blank` — a real window
is never one flat colour) and falls back to copying the same rectangle off the
screen, which needs the window unobstructed but always shows what is there.
Which route a capture took is reported on the `Capture`, so a test can say so.
In practice `PrintWindow` worked on every capture taken below.

#### Verified against the built `suite.exe`

Two sandboxed instances driven over the socket: `assets/sample.xlsx`, and a
chart-bearing corpus workbook.

| Asked | Answered |
|---|---|
| `window` | `1180x800 at (370,136)` — the client area |
| `grid` | `1180x534 at (370,350)` |
| `cell:A1` | `104x24 at (416,350)` — `370 + SHEET_GUT`, on the nose |
| `cell:B3` | `65x24 at (520,398)` — one column right, two rows down |
| `cell:A1:C5` | `252x120` — three columns by five rows |
| `chart:0` | `1170x506 at (481,416)` |
| `chart-panel` | `231x607 at (1319,303)` |
| `chart-panel`, none open | "the Chart panel is not open" |
| `chart:0`, no charts | "this sheet has no charts" |
| `chart:5` of one | "no chart 5; this sheet has 1 (0..0)" |
| `nope` | "unknown region 'nope' (window, grid, chart-panel, ...)" |
| `cell:A0`, `cell:`, `chart:x` | each refused by name |
| `cell:BB2` | "column BB is scrolled off to the right of the grid's view" |

The PNGs were **opened and looked at**, not just measured: the `window` capture
is the app pixel for pixel; `cell:A1:C5` is exactly the Item/Qty/Unit-price
block and nothing else; `chart-panel` is the panel from its "Chart" heading down
to the second series' colour swatches. `chart:0` came back `1077x506` rather
than `1170` with the panel open — the card runs past the window edge, so the
crop clamped, which is the partly-outside path working on a real region.

That last table row is a change made *because* of what a run showed:
`col_span_x` walks as far right as it is asked, so a column past the viewport
had a position that was not on screen. Reporting it handed back a rectangle
outside the window, and the crop then failed with a message about pixels
instead of about the column.

**A correction to Task 3's scope note.** It recorded that no fixture in the repo
carries a chart. That is true of `assets/` and `corpus/xlsx/`, but *not* of
`corpus/xlsx-ext/libreoffice/chart2/` — which is where the chart workbook used
above came from, and which is how `chart:0`, `chart-panel` and `select-chart`
all got a success path here. Task 6 can draw its chart fixture from there.

**A footnote on the gpui finding, for Task 8.** `Window::render_to_image` exists
and sounds like the readback this plan says is absent. It is behind
`cfg(any(test, feature = "test-support"))` and re-renders the scene to an
offscreen texture rather than reading what the compositor put on screen, so it
is not available in a shipping build and would not show what a user sees. The
plan's conclusion stands; the reason is narrower than "there is no such API".

68 new unit tests (35 in the suite crate, 33 in `uiharness`); suite 178 passed,
`uiharness` 33, `gridcore` 370, `ctlcore` 30, `opccore` 27; clippy `-D warnings`
and `cargo fmt --check` clean in both workspaces.

### Task 5: Assertions about what was drawn

- [x] implement the pixel probes the first tests need: sample a straight run of
      pixels along an edge and decide **solid vs dashed vs absent**, and match a
      colour within a tolerance so anti-aliasing does not fail a true result
- [x] express expectations in the test's own vocabulary — `border A1:C5 solid`,
      `border A1:D5 dashed teal` — resolving to a rect via Task 4 and a probe
      via this task
- [x] on failure, report what was expected, what was observed, and the path to
      the PNG. A failure that only says "assertion failed" would send the reader
      back to installing a build, which is what this plan exists to avoid
- [x] write tests for the probes over synthetic images: a solid line, a dashed
      line at the shader's `2W` dash / `1W` gap pitch, an empty edge, and an
      anti-aliased edge that must still read as its nominal colour
- [x] run tests — must pass before Task 6

#### The probe: a band, not a row

`uiharness/src/probe.rs`. A region's rectangle is the region's own box, and the
app draws its border straddling that box — inset `-1px`, `2px` wide — so of the
two pixel rows the top border covers, exactly one is inside the crop. Rather
than have every caller know that inset, `probe_edge` scans a **band** four
pixels inward and keeps the line that best matches what was asked for. A crop
one pixel off still reads the right line; an edge with nothing on it still reads
absent, because nothing in the band matches.

That band is also what makes the anti-aliasing requirement fall out rather than
need handling. A 2px line that does not sit on a pixel boundary lands as a
half-covered row, two full ones, and another half-covered row. The half rows are
further from the nominal colour than any sane tolerance, the full ones are it
exactly — so keeping the *best* line keeps the core, and the colour reported is
the one the renderer asked for rather than a blend of it with the paper.

`classify` is the pure core and takes a hit mask, not pixels:

| Reading | When |
|---|---|
| `Absent` | ≤10% of the run |
| `Solid` | one piece, ≥90% |
| `Dashed` | ≥3 pieces, 25–90%, dashes and gaps each within 3× of each other |
| `Broken` | anything else |

One pixel of drop-out is closed before counting (anti-aliasing, not a dash — the
shader's own gap is `1 × width` = 2px, so closing 1px cannot flatten a real
dashed line), and pieces under 2px are dropped as speckle.

**`Broken` is the fourth answer, and it is why the thresholds are what they
are.** The plan asked for three; a line with a hole in it is none of them, and
the first cut of `classify` called it dashed — two pieces covering 75% of the
run looks exactly like a dash pattern to a rule that only counts pieces. It
passed a test that ought to have failed, which is the worst thing a harness can
do. Hence `DASH_MIN_SEGS` (a pattern needs at least two gaps to repeat; the
shortest edge the grid draws is a 21px row, which still shows three or four
dashes) and `DASH_SPREAD` (three unequal fragments are fragments). A reading
nobody predicted is now reported as itself rather than rounded to the nearest
thing the test was hoping for.

#### The vocabulary

`uiharness/src/expect.rs`, and `uiharness --config <sandbox> assert <expectation>`.

```text
border <region> [<side>…] <solid|dashed|absent> [<colour>]
no border <region> [<colour>]
```

The region is Task 4's — plus writing `A1:C5` for `cell:A1:C5`, because a test
about a selection should read like the selection. Only a real A1-style reference
is rewritten: a bare word wrongly turned into `cell:` would quietly ask about
the wrong thing, whereas a typo left alone comes back from the app as an error.
Sides are optional and default to all four; the style words may come in either
order after the region. Colours are named (`teal` = `BRAND`, `blue`/`purple`/
`green` = the chart slot colours) or `#rrggbb`, and the names are pinned to
`main.rs`'s own constants by a test — a test that passes against the wrong
colour is testing nothing.

`absent` **with** a colour is a real expectation and the useful one: "no teal
line here", which leaves any other line on the edge alone. See the run below.

#### Verified against the built `suite.exe`

A sandboxed instance on `assets/sample.xlsx`, `drag A1 -> C5`, then asserted.
This is the plan's own regression case: a swept range wears a **solid** border.

| Asked | Answered |
|---|---|
| `border A1:C5 solid teal` | ok — all four edges `#2aa79b`, 98–100% of 248/116px |
| `border A1:C5 dashed` | FAILED, each edge `solid (100% of 248px) #2aa79b` |
| `border A1:C5 solid blue` | FAILED, `absent` — and under each, `what is drawn there: solid … #2aa79b` |
| `no border H20 teal` | ok — an unselected cell |

The colour miss is the one worth reading twice. Asking for blue and finding
nothing is true but useless on its own, so a side that fails a *coloured*
expectation is probed a second time against the background and the report says
what is actually drawn there. Without that, a wrong-colour border and an absent
one produce the same message.

**And one finding that changes how a test should be written.** `no border H20`
— no colour — FAILS on a perfectly ordinary cell, with `solid (100% of 61px)
#d9d9d9` on its right and bottom. Those are the sheet's own gridlines. The
nameless form asks "is there **any** line here", which is a real question for a
region that should be blank, but it is not the question "is this cell
unselected". That one is `no border H20 teal`. Recorded in `expect.rs`'s module
note, because it is the kind of thing that is obvious once and never again.

69 tests in `uiharness` (36 new), suite 178, clippy `-D warnings` and
`cargo fmt --check` clean.

### Task 6: The script format, and the first real cases

- [x] define the test script format and write its parser as a pure function
- [x] write the two regression cases from the Overview: a drag-to-select that
      must not fill and must not dash, and a pointed range that must dash
- [x] add a case for one selection at a time: with a cell being edited, select a
      chart, and assert the keyboard goes to the chart — the invisible-caret bug
      that swallowed every keystroke
- [x] add a fixture workbook under the harness's own directory; it must not read
      anything from the user's documents
- [x] write tests for the script parser: a valid script, an unknown verb, a
      malformed assertion, and a script that references an undefined region
- [x] run the harness end to end and record in this plan what it actually
      caught, including any case that could not be expressed
- [x] run tests — must pass before Task 7

#### The format

`uiharness/src/script.rs`, run with `uiharness run <script.uit>`. Line-based,
one file of cases, `#` to end of line is a comment:

```text
test drag-to-select does not fill
  open ../fixtures/basic.xlsx
  click A1
  snapshot A1:C5
  drag A1 -> C5           # a sweep, not a fill
  assert range is A1:C5
  assert no fill preview
  assert cells unchanged
  shot cell:A1:C5
```

Steps are `open`, `click <cell> [shift] [double]`, `drag <a> -> <b>`, `type`,
`key`, `select chart <n>`, `focus <field>`, `snapshot <range>`, `shot <region>`
and `assert`. Assertions are Task 5's border vocabulary, plus `<key> is [not]
<value>` over the app's own state reply, `cell B2 is 99`, and the two phrases
the plan's own example script used — `no fill preview` and `cells unchanged`.

Those last two are not sugar for one state key, and the pair is the point: an
auto-fill that *ran* leaves `filling` false again afterwards and only the cells
show it, while one that is merely *armed* shows in `fill_preview` with no cell
changed yet. The original regression went in through one and out through the
other.

`parse_script` is pure and the whole file is parsed — including its region
names, which the single-shot `assert` path deliberately leaves to the app — so
a typo costs milliseconds instead of a cold start on step 9 of case 3.
`uiharness run` also starts and stops its own sandboxed instance
(`launch.rs`), so a case never runs against a window somebody had been clicking
in.

#### What running it actually caught

Five cases in `uiharness/cases/sheet-selection.uit`, against the built
`suite.exe` on the harness's own `fixtures/basic.xlsx` (generated by
`tests/fixture.rs`; `UIHARNESS_REGEN_FIXTURE=1` rebuilds it). All five pass
now. Three of them did not at first, and **none of the three was the app being
wrong about the thing the case was written for** — which is its own result:

1. **A modal froze the whole run, and looked like a hang.** Case 4's `open` of
   the fixture timed out with nothing on stderr, and the window still answered
   Windows messages, so it looked alive. `open_args` (main.rs:9446) shows an
   `rfd::MessageDialog` when the file is already open and *dirty* — and case 3
   dirties the tab by pointing a chart field at some cells. `rfd` runs its own
   modal loop on the app thread, which stops the control pump dead. Fixed in
   the app: the prompt is skipped when `self.harness.is_some()`, answering "no"
   (keep what is open) for it, so `open` in a harness instance means "make this
   file the active tab". **A harness instance must never be able to block on a
   dialog nobody can answer**; this is the only such prompt on the verbs' paths,
   and any new one needs the same guard.

2. **Task 5's `dashed` reading could not read the app's own dashed border.**
   The real thing measured `broken (20 runs of 2-7px with gaps of 3-3px,
   covering 52% of 126px)`. The gaps are perfectly even — it is the *dashes*
   that vary, because the grid renders the border **cell by cell**, so the dash
   phase restarts at every cell boundary: the last dash before a join is cut
   short and the first two after it abut. `is_regular`'s shortest-to-longest
   rule called that fragments. It now measures the spread over the middle of the
   distribution (`TRIM_FRACTION`, one in six set aside at each end), which is
   zero for the small counts the rule was written for — three uneven fragments
   still read `Broken`.

   That loosening needed a counterweight, and the same run supplied it: the
   diagnostic probe against the *background* reads 87% coverage in 8 runs on
   even gaps, which a piece-counting rule would happily call dashed. gpui's
   pattern is fixed at `2W` on / `1W` off, so two thirds is the *ceiling* for
   this shader — hence `DASH_MAX = 0.80`, and a run covering 85% in eight pieces
   stays `Broken`. `Broken` also now says what it was rejected on (`runs of
   2-7px with gaps of 3-3px`), because "20 runs covering 52%" reads exactly like
   a dash pattern to anyone holding the message rather than the pixels.

3. **A `#rrggbb` was being eaten as a comment.** `assert border A1:B4 solid
   #2f6fdb` parsed as `assert border A1:B4 solid` — and *passed*, about a weaker
   thing than the case said. Found by reading the transcript of a case that
   passed, which is the argument for the runner printing every step and not only
   the failures. `#` now only starts a comment when it is not the head of a
   six-digit hex colour.

**And one case that could not be expressed as written.** The plan's second
regression case says "a pointed range that must dash". The first draft pointed
by typing `=SUM(` into a cell and dragging, and it failed: `picking` was false
and the border came back **solid `#2f6fdb`**. That is not a bug. `ov.picking` is
`range_field_active()` — a *reference field* holding the keyboard, the chart
panel's or an entry bar's — and a formula being typed into a cell is the other
kind of pointing, which the grid marks with the formula's own coloured reference
wash instead. So the case became two: `a pointed range dashes` focuses
`chart-range` and drags, and `a formula reference is washed, not dashed` keeps
the original steps with the expectation the app actually meets. Both are worth
having; the second is where the wrong assumption was.

#### Scope changes

- `uiharness` gained a **dev**-dependency on `gridcore`, for `tests/fixture.rs`
  only. The binary still depends on `ctlcore` and `opccore` alone — it reads
  pixels and JSON and wants no spreadsheet engine — but hand-writing an xlsx
  with a chart part in it would have been six XML files nobody would check.
- `launch.rs` (starting and stopping a sandboxed instance) was not in the plan.
  Without it a case would have to attach to whatever was already running, which
  is a test of that instance's history rather than of the app.
- The `open_args` modal guard is a change to the **app**, not the harness. It is
  recorded above rather than deferred because a control surface that can be
  wedged by a dialog is not a control surface. It has no unit test of its own —
  the guard is one condition on a method that needs a live window — and is
  covered by the case file, whose fourth case is the run that found it.

104 tests in `uiharness` (35 new: 12 for the script parser, 12 for the runner's
comparisons, 3 for the launcher, 2 for the new probe readings, plus the fixture
and committed-case checks); suite 178, `gridcore` 370. Clippy `-D warnings` and
`cargo fmt --check` clean in both workspaces.

### Task 7: Verify acceptance criteria

- [x] verify a harness run cannot touch the real config: run it, then confirm
      the real `session.json` and `hot/` are byte-identical to before
- [x] verify the suite with no harness flag is unchanged — real config root, no
      listener, no discovery file
- [x] verify each test case from Task 6 fails when its fix is reverted, which is
      the only proof a test tests anything
- [x] run all tests in both workspaces, clippy and fmt clean

#### Isolation, measured against the real profile

`%APPDATA%\docxy` was hashed (SHA-256 per file) and its mtimes recorded before
anything ran: nine files, including four `hot/tab-N.{docx,xlsx}` sidecars of the
user's own open documents. Then the full case file was run
(`uiharness run uiharness/cases/sheet-selection.uit`), which starts a sandboxed
instance, opens the fixture, edits cells and quits it.

Afterwards the real directory was **byte-identical and mtime-identical** — not
one file touched. The same writes the app would have made landed in the sandbox
instead:

```
target/uiharness-runs/sandbox/docxy/session.json
target/uiharness-runs/sandbox/docxy/hot/tab-0.docx
target/uiharness-runs/sandbox/docxy/hot/tab-1.xlsx
```

That pair is the whole proof: the run *did* persist, and it persisted somewhere
else. A run that simply never wrote would show the same untouched profile and
would prove nothing.

#### The no-flag build, checked against the same directory

Reading a config root off a process that has no control surface means watching
what it writes, so this one is destructive by nature: the real directory was
copied out, hashed, and the copy verified identical first. `suite.exe` was then
started with **no `--harness` and `DOCXY_CONFIG_DIR` explicitly removed from its
environment**, left up for 12s, and killed.

| Asked | Answered |
|---|---|
| does it use the real config root? | yes — `session.json` and all four `hot/tab-N` sidecars rewritten in `%APPDATA%\docxy`, mtimes moved |
| does it start a listener? | no — `%APPDATA%\suite\ctl` was never created (that is `control_dir(config_root)`, and also where `ctlcore::config_ctl_dir("suite")` would land) |
| does it write a discovery file? | no — the after-hash listing has the same nine files as the before one, so nothing new appeared anywhere under the root |

The contents were unchanged as well (only the mtimes moved — the app rewrote the
sidecars to the same bytes), and the backup was restored and re-verified
identical in both hashes and mtimes, before and after every later step.

#### Every case fails when the thing it guards is put back

A case that passes proves nothing until it has been made to fail. Five fixes
were reverted behind one `UIHARNESS_BREAK=<name>` switch — a temporary patch,
built once, run five times, then reverted and rebuilt — so each mutation is the
only difference between a red run and a green one. A control run of the mutated
binary with the variable unset passed all five, which is what makes the switch
itself trustworthy.

| Mutation | Reverts | Case | What failed |
|---|---|---|---|
| `sweep-fills` | a sweep arms the auto-fill instead of selecting | 1 | `cells unchanged`: *A2 was 'North' and is now 'Region'*, …A5 — and `range is A1:C5` observed `A1:A5` |
| `dash-always` | `border_is_dashed` ignores `pointing` | 2 | `border A1:C5 solid teal`: *top dashed (30 dashes of ~3.3px, gaps ~3.0px, 52% of 191px)* |
| `dash-never` | it never dashes | 3 | `border A1:B4 dashed teal`: *top solid (100% of 126px)* |
| `formula-dashes` | a formula pick counts as pointing | 4 | `picking is false`: observed true |
| `keep-edit` | a chart press leaves the cell edit open | 5 | `editing is false`: observed true |

Three of those runs failed **only** the case they were aimed at; `sweep-fills`
also took case 2 down, because a fill leaves the selection `A1:A5` and case 2's
border is asked of `A1:C5`.

**Two findings worth more than the four ticks.**

1. **Case 1 survives its own regression only because of `cells unchanged`.**
   Under `sweep-fills`, `assert no fill preview` and `assert dragging is false`
   both **passed** — by the time the case asks, the fill has committed and
   cleared `fill_preview` again. Exactly the split Task 6 predicted: an armed
   fill shows in `fill_preview`, a fill that *ran* shows only in the cells. Drop
   `snapshot`/`cells unchanged` from that case and it goes green on the very bug
   it was written for.

2. **Case 4's pixels are not what catches `formula-dashes`; its state
   assertion is.** With a formula pick counted as pointing, `picking` flips but
   `border A1:B4 solid #2f6fdb` still **passes** — `border_range` needs a
   multi-cell selection or a range-field preview, and a half-typed `=SUM(` in
   E2 has neither, so no dashed border is ever drawn to be seen. The case bites
   through the pair, not through the picture.

#### Green in both workspaces

`suite` 178 tests, `uiharness` 104, `gridcore` 370 + its integration tests, all
passing; `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`
clean in both. The five cases pass against the rebuilt clean binary, and the
real profile is still byte-identical to the hash taken before any of this ran.

### Task 8: [Final] Document it

- [x] document how to write and run a harness test, the verbs and regions
      available, and the isolation guarantee and how it is enforced
- [x] record the gpui-has-no-readback finding so the next person does not go
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
