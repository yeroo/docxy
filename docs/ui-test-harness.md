# The scripted UI test harness

Every visual change in the desktop suite used to be verified by a person
installing a build and looking at it. That is why two regressions — a
drag-to-select that ran auto-fill, and dashes on an ordinary selection —
reached an installer before anyone noticed. Neither was reachable from a unit
test: both live in event handling and rendering, and gpui `#[test]` blows up the
moment a view is constructed.

The harness drives the app by verbs, screenshots it, and asserts on the pixels,
in an instance that **cannot touch the real one**.

```text
test drag-to-select does not fill
  open ../fixtures/basic.xlsx
  click A1
  snapshot A1:C5
  drag A1 -> C5
  assert range is A1:C5
  assert no fill preview
  assert cells unchanged
  shot cell:A1:C5
```

Two halves, in two workspaces:

| Half | Where | What it does |
|---|---|---|
| the app side | `suite/docxy/src/harness.rs` | an opt-in [`ctlcore`](../ctlcore) server: the verbs, the state reply, and the region geometry |
| the driver side | `uiharness/` (root workspace) | the script format, the launcher, `PrintWindow` capture, the pixel probes, the runner |

## Running the committed cases

```bash
cargo build --release --manifest-path suite/Cargo.toml   # the harness drives a built suite
cargo run -p uiharness -- run uiharness/cases/sheet-selection.uit
```

That one command starts a sandboxed instance, runs every case in the file
against it, files a PNG per `shot`, and stops it again. It exits non-zero if any
case failed. It needs a **desktop session** — `PrintWindow` has nothing to draw
into without one — so this is a local, on-demand tool, not a CI step.

Output is a line per step, passing or not, because a transcript that mentions
only the failure leaves you guessing whether the setup even worked:

```text
case: drag-to-select does not fill — FAILED
  ok    open ../fixtures/basic.xlsx
  ok    drag A1 -> C5
  FAIL  assert no fill preview
        expected: no fill armed and nothing previewed
        observed: filling=true, fill_preview=A1:C5
```

Useful flags on `run`:

| Flag | Effect |
|---|---|
| `--suite EXE` | drive this binary. Otherwise `$UIHARNESS_SUITE` if it is set (an error if it does not name a file), otherwise the first build found — release before debug, under `suite/target/` then `target/`, relative to the repository this crate was built from and then the working directory. Release wins over debug whatever their dates, so every run prints the binary it drove as its first line — that is what makes a stale release build answering for a fresh debug one visible |
| `--run DIR` | where evidence is filed (default `./uiharness-runs`, which is git-ignored) |
| `--sandbox DIR` | the throwaway config root (default `<run>/sandbox`, **erased before each run**; a directory you name yourself is yours to manage, and is kept) |
| `--keep` | leave the instance up after the script ends, to poke at the window a case failed on |

### Evidence

Every capture lands under the run directory in a folder named for the test that
took it, so a failing case can be looked at afterwards:

```text
uiharness-runs/drag-to-select-does-not-fill/007-cell-a1-c5.png
```

Names are slugged (`cell:A1:C5` → `cell-a1-c5`) — a colon in a Windows path is a
stream separator, and a capture named straight from a region would silently
write somewhere else. The number is the step's line in the script: two steps may
look at the same region, and without it the later capture would overwrite the
one the failure above it points at.

## Driving one by hand

Working a case out interactively beats guessing at it. Start an instance
yourself, then attach:

```bash
DOCXY_CONFIG_DIR=/tmp/sandbox suite.exe --harness
uiharness --config /tmp/sandbox ping
uiharness --config /tmp/sandbox call drag '{"from":"A1","to":"C5"}'
uiharness --config /tmp/sandbox rect cell:A1:C5
uiharness --config /tmp/sandbox shot grid --test scratch
uiharness --config /tmp/sandbox assert border A1:C5 solid teal
```

`--config` is the sandbox the instance was started with; the control directory
is derived from it exactly as the app derives it, so the two cannot end up
looking in different places. It defaults to `DOCXY_CONFIG_DIR` in your own
environment. `--ctl DIR` names the control directory outright.

`DOCXY_HARNESS=1` is equivalent to `--harness`, for a launcher that cannot add
an argument.

## Writing a case

A script (`*.uit`) is plain text. `#` starts a comment — except inside `type`,
whose text is taken verbatim, because `#` is a character a spreadsheet test has
every reason to type. A `test <name>` line starts a case and everything under it
belongs to that case. Two cases may not share a name; their captures would land
on top of each other.

`parse_script` is a **pure function over the text**, and the runner parses every
file before it launches anything — so a typo costs milliseconds rather than a
cold start, and every accepted and rejected form is a unit test.

### Steps

| Step | Drives |
|---|---|
| `open <path>` | the file, resolved **against the script's own directory** — never the working directory |
| `click <cell> [shift] [double]` | the cell's click handler (press, click, release) |
| `drag <from> -> <to>` | press, one move per cell crossed, release. `to` and a bare space read the same |
| `type <text>` | one key event per character. The text is taken verbatim between its ends; the whitespace on either side of it is trimmed, so `type   =SUM(` types `=SUM(` |
| `key <k> [k…]` | those keys, in order: `escape`, `enter`, `tab`, `up`, `f2`, `ctrl+c`, `shift+down`, … |
| `select chart <n>` | the press on a chart card, counting from 0 |
| `focus <field>` | the click on a reference field |
| `snapshot <range>` | remembers those cells, for a later `assert cells unchanged` |
| `shot <region>` | files a PNG of the region |
| `assert …` | see below |

Reference fields, for `focus`: `chart-range`, `chart-title`, `categories`,
`series-name:N`, `series-values:N`, `cond-format`, `validation`, `sort`,
`text-to-columns`.

**`open` always loads the file from disk.** The cases in a script share one
instance — a process per case would multiply a two-second launch by however
many cases there are — so `open` is the only setup a case has, and it has to
mean the same thing on case 5 as it did on case 1. In a harness instance it
therefore replaces the tab whether or not it has unsaved edits, silently. Not
everything a case leaves behind sets the dirty flag — an uncommitted in-cell
edit, the selection, the scroll position — so reloading only the dirty ones
would carry the rest into the next case, and a case would pass or fail on the
order it ran in. (A normal instance still asks before discarding unsaved work,
and still keeps it if you say no.)

⚠️ **No modal dialog may sit on a path a verb can reach.** `rfd` runs its own
message loop on the app thread, which stops the control pump dead — the window
keeps answering Windows messages so it *looks* alive, while every verb after it
times out with nothing on stderr to say why. The reload prompt, the Save As
dialog a `key ctrl+s` on a never-saved workbook would raise, and the
unsaved-changes prompt on close are each gated on `harness.is_none()`; anything
new in that family needs the same guard, and should refuse in words instead
(`tab.status`), which a case can read.

### The fixture

Cases run against `uiharness/fixtures/basic.xlsx`: one sheet, a `Region/Q1/Q2`
header and four rows in `A1:C5`, and one bar chart anchored below and right of
`A1:D5` so it never sits under a drag. It is committed — a UI test must not
depend on a build step running first — but generated from literals in
`uiharness/tests/fixture.rs`. Change `build()` there and regenerate, or the
drift check fails:

```bash
UIHARNESS_REGEN_FIXTURE=1 cargo test -p uiharness --test fixture
```

Nothing in the harness reads a user document, which is the point of the fixture
living under the harness's own directory.

Every verb goes in through **the same entry point the pointer or keyboard
would**. A verb that reached past a handler into the state it maintains could
pass while the handler under test was broken — the one way this harness could be
worse than nothing.

### Assertions

| Assertion | Reads |
|---|---|
| `border A1:C5 solid teal` | the pixels |
| `border A1 top dashed` | the pixels, one named edge (`top`, `right`, `bottom`, `left`) |
| `no border H20 teal` | the pixels: no such line on any edge |
| `<key> is [not] <value>` | one key of the app's state reply |
| `cell B2 is [not] <text>` | what that cell shows |
| `no fill preview` | `filling` and `fill_preview` together |
| `cells unchanged` | every cell of the last `snapshot` |

State keys, as the app reports them after every driving verb:

| Key | |
|---|---|
| `tab`, `title`, `dirty`, `sheet_tab` | the active tab |
| `sheet`, `sel`, `anchor`, `range` | the sheet and its selection |
| `editing`, `edit` | whether a cell edit is open, and its text |
| `chart_sel`, `panel_chart`, `charts` | chart selection and the panel |
| `field`, `field_text` | the focused reference field, and its buffer |
| `filling`, `fill_preview`, `dragging` | the auto-fill and the sweep |
| `picking`, `range_preview`, `sel_hidden` | point mode |

`nothing` is how a script writes "this key is null" (`assert chart_sel is
nothing`); `null` and `none` read the same, and `empty` matches an empty string.
Comparison is case-insensitive, and `is not` negates.

### Regions

`window`, `grid`, `chart-panel`, `cell:B3`, `cell:A1:C5`, `chart:0`. Inside a
border assertion the `cell:` may be dropped — `border A1:C5 solid` — because an
assertion about a selection should read like the selection.

**The app answers where a region is; the harness takes the picture.** Only the
layout knows where the grid starts, where `A1` is at this scroll position, or
how big a chart card became, so the app answers the `rect` verb in physical
desktop pixels and the harness crops its capture to that. A harness that
computed "the grid starts 120px down" would break on a ribbon change and would
have to be corrected for DPI.

Geometry and pixels are read from a **settled** frame. A verb only marks the
view dirty, so when its reply goes out the frame that shows what it did has not
been laid out yet; the driver reads the frame counter, sends its verbs, then
waits for `rect` to report a higher one before capturing.

**A region that is only partly on screen is refused, not guessed at.** Both
sides of the crop enforce this, because a half-visible edge is the one failure
mode a pixel assertion cannot notice by itself: the picture is a perfectly good
picture, the probe reads a perfectly good line, and the verdict is about the
wrong pixels.

- The app refuses `rect` for a range whose row or column is cut by the edge of
  the grid — `column C is only partly in the grid's view; scroll it fully into
  view first`. Its border is not drawn where a probe would look for it, so
  there is no honest answer to give.
- The harness clamps a crop that runs off the *window* (there are still pixels
  worth filing as evidence) but records which edges it moved, and a border
  assertion then refuses those edges rather than reading the window's frame as
  if it were the region's.

### Colours

`teal`/`brand` (`#2AA79B` — the selection ring and the pointed range's dashes),
`blue` (`#4472C4`), `purple`, `green`, `white`, `black`, or any `#rrggbb`.

**Name the colour when you mean "no selection here."** An expectation with no
colour asks "is there *any* line here", measured against the region's own
background — and an ordinary cell has the sheet's gridlines along two of its
sides, so `no border H20` fails on a perfectly ordinary cell with `solid (100%
of 61px) #d9d9d9`. That is the truth about the picture, but not what the test
meant. `no border H20 teal` is. The nameless form still earns its keep for a
region that should be blank — a fill preview that must not have been drawn, say.

### Two things learned by making the cases fail

Each of the committed cases was verified by reverting the fix it guards and
watching it go red. Two of them turned out to bite through something other than
what they looked like they bit through:

1. **`assert cells unchanged` is what catches the auto-fill regression**, not
   the pixels. Under a sweep that fills, `assert no fill preview` and `assert
   dragging is false` both *pass* — by the time the case asks, the fill has
   committed and cleared `fill_preview` again. An armed fill shows in
   `fill_preview`; a fill that *ran* shows only in the cells. Drop the
   `snapshot`/`cells unchanged` pair and the case goes green on the very bug it
   was written for.
2. **A state assertion can be the only thing holding a pixel case up.** With a
   formula pick wrongly counted as point mode, `picking` flips but `border
   A1:B4 solid #2f6fdb` still passes — a dashed border needs a multi-cell
   selection or a range-field preview, and a half-typed `=SUM(` in E2 has
   neither, so no dashes are ever drawn to be seen.

Assert the state *and* the picture. Either alone has a way to be quietly right.

## The isolation guarantee

**A harness instance cannot write to the installed app's config.** This is the
requirement the harness is built around, not a nicety: the suite hot-persists
open documents regardless of whether they were saved, so a test instance sharing
that directory would overwrite documents the user has open.

`--harness` is honoured **only** when `DOCXY_CONFIG_DIR` names a directory that
is not the real config root. `harness::gate` refuses otherwise, and it refuses
in the process that would do the damage — before a listener exists, so the
symptom is the driver's connect timing out and the cause is on the child's exit.
The launcher sets the variable and then trusts that refusal; a second check on
the driver side would be an opinion that can drift from the first.

### The trap that makes isolation look complete when it is not

There are two different path-resolution mechanisms in play:

| Path | Resolved by | Honours `APPDATA`? |
|---|---|---|
| `ctlcore::config_ctl_dir` | reads `APPDATA` directly | yes |
| `dirs::config_dir()` | the Windows known-folder API | **no** |

So setting `APPDATA` for the child isolates the control socket while
`session.json` and the hot sidecars still land in the real profile. That is why
the sandbox is a dedicated variable the app reads itself, and why the discovery
file goes under `<sandbox>/suite/ctl` — derived from the sandbox root — rather
than through `ctlcore::config_ctl_dir`, which would do its own `APPDATA` lookup
and could put the socket in a different sandbox from the session state.

### How it was measured

`%APPDATA%\docxy` was hashed per file and its mtimes recorded — nine files,
including four `hot/tab-N.{docx,xlsx}` sidecars of the user's own open
documents. The full case file was then run. Afterwards the real directory was
**byte-identical and mtime-identical**, and the same writes the app would have
made had landed in the sandbox instead:

```text
target/uiharness-runs/sandbox/docxy/session.json
target/uiharness-runs/sandbox/docxy/hot/tab-0.docx
target/uiharness-runs/sandbox/docxy/hot/tab-1.xlsx
```

That pair is the whole proof: the run *did* persist, and it persisted somewhere
else. A run that simply never wrote would show the same untouched profile and
would prove nothing.

The no-flag build was checked against the same directory: started with no
`--harness` and `DOCXY_CONFIG_DIR` removed from its environment, it used the
real config root as it always has, started **no listener** (`%APPDATA%\suite\ctl`
was never created) and wrote **no discovery file**. Without the flag none of this
runs; the harness is opt-in per process, never a mode a user can end up in by
accident.

The committed cases are also guarded by a unit test that every `open` in them
resolves to something under the harness's own tree — a case that reached into
the user's documents would pass on the machine it was written on and nowhere
else.

## gpui has no window readback — do not go looking for a screenshot API

This costs an afternoon to rediscover, so: **gpui cannot hand you the pixels of
a shipping window.** The three things that sound like they can, and why they
cannot:

| Looks right | Actually |
|---|---|
| `App::screen_capture_sources` | the screen-*sharing* source list — whole displays, not a window |
| `to_image_data` | renders SVG |
| `Window::render_to_image` | behind `cfg(any(test, feature = "test-support"))`, and re-renders the scene to an offscreen texture rather than reading what the compositor put on screen |

So the app reports geometry and the harness takes the picture, with Win32
`PrintWindow` (`PW_RENDERFULLCONTENT`) against the test window's HWND, found
from its process id. `PrintWindow` asks the window to draw itself into a device
context, which gets that window alone even when another is in front of it — a
test therefore does not have to own the desktop.

Its weakness is the mirror image: a window drawn by the GPU (gpui uses DirectX)
sometimes comes back blank, because the flag only reaches DWM-redirected
content. When that happens the capture falls back to copying the same rectangle
off the screen, which needs the window visible and unobstructed but always shows
exactly what is there. Which route a capture took is on the `Capture` it
returns, and the CLI prints it (`via PrintWindow` / `via Screen`) — worth reading
when an assertion fails in a way that makes no sense.

There is no image crate in the dependency list either: `uiharness` writes real
PNGs through its own DEFLATE (`deflate.rs`, `png.rs`, round-tripped in tests
against `opccore`'s inflater). Its only dependencies are `ctlcore` and `opccore`
from this repo, `windows` for the Win32 capture, and `gridcore` as a
dev-dependency solely to generate and check the fixture workbook — the binary
itself has no spreadsheet engine in it and wants none, since it reads pixels and
JSON.

### Why not golden images

Considered and rejected. Reference PNGs diffed wholesale catch every unintended
change, but they are brittle across GPU, DPI and font differences, and the
practical failure mode is that a diff gets blanket-approved rather than
investigated. Targeted probes state what is being asserted, so a failure names a
property — "top dashed (30 dashes of ~3.3px, gaps ~3.0px, 52% of 191px)" —
rather than a pixel count. The full PNG is saved regardless, so nothing stops a
human looking at the whole picture.

## Tests of the harness itself

```bash
cargo test -p uiharness
cargo clippy -p uiharness --all-targets -- -D warnings
cargo test --manifest-path suite/Cargo.toml     # the app side: the gate, region and key parsing, the config root
```

The decisions live in pure free functions — parsing a script, resolving a region
name, deciding whether a sampled row of pixels is dashed or solid, comparing an
expectation to an observation — and those are what the unit tests cover. The
harness end to end is exercised by running it.

## Not covered

- **CI.** It needs a desktop session for `PrintWindow`.
- **The Doc and Mail tabs.** The verbs are the sheet UI's, which is where the
  regressions have been.
