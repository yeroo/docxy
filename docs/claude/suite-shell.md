# Suite shell

Notes moved from `CLAUDE.md` (#1903). Read them before changing the suite's window chrome: the start screen, the title bar label, the accent and the theme command. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Start screen (#806)

- A suite launch with nothing to restore and no file on the command line has
  no tab and the File > New page up (`Docxy::show_start_screen`; `startup_tabs`
  decides, `Startup::StartScreen`), brought up through `open_backstage` so the
  lists are filled. A restored session keeps its tabs; files on the command
  line open over it (a file that cannot be read opens as a failed-load tab, so
  one tab always exists). With no tab, Back on File goes from New to
  its default page (Recent, Settings) and stay in File, Close, Save and Save
  As do nothing, and the startup plugin notice waits in `pending_status` for the first
  tab (`land_status`; a tab's own status wins). A failed Open pick with no tab
  keeps File up and says why under Recent (`recent_message`). The New page's cards are `NEW_CARDS`
  (the Spreadsheet one reads "Blank workbook").
- A harness instance keeps the sample document as tab 0 (the cases assume it)
  unless the script says `start screen`: the runner writes the marker
  `<config root>/docxy/start-screen` into the sandbox, which only a harness
  instance reads (`harness::start_screen_requested`). `start-screen.uit` drives
  it (`backstage` read reports `new` and `cards`, `new-card` clicks one).
- `Docxy` cannot be built in a unit test (no gpui test context), so the
  zero-tab guards are checked by reading the sites that index
  `self.tabs[self.active]`: each follows a `tabs.get`/`is_empty` guard, and
  `save_project_to`, the one that did not, now refuses with "no tab is open".

## Title label and accent per tab type (#1473)

- The suite's title bar names the selected tab's type in one word (`doc`,
  `sheet`, `project`; Mail `mail`, no tab `doc`), not the document, whose name
  stays on its tab. The OS window title is unchanged (`doc_name::window_title`).
- `suite/docxy/src/accent.rs` holds the accent per type and theme (doc keeps
  `BRAND`, sheet green, project yellow; the sheet and project shades hold white
  File text at 4.5:1). `Docxy::render` calls `accent::set` before building any
  chrome and the ribbon, backstage and title bar read `accent::brand()`; every
  other `BRAND` use (dialogs, comment and find bars, carets, Gantt) is not
  accent. A `.hover` style is built at once, so it reads the accent during
  render too. `accent_now` takes the theme from the preference, as render sets
  the accent before `apply_theme`. `tab-list` reports `title_label`, `accent`
  (from the tab) and `drawn_accent` (what this window's last render set); `uiharness/cases/title-accent.uit` asserts them.
- The Light/Dark/Auto theme button is not in the title bar: View > Theme
  (doc `Act::DarkMode`, `SheetAct::Theme`, `ProjectAct::Theme`) and Backstage's
  Settings Theme row (always reachable: the File button is drawn in every state,
  and the only place for Mail, a failed load, no tab, Protected View and a
  minimised ribbon, where `ribbon_body_hidden` draws no View tab) call
  `cycle_theme`, which says the new setting on the status line.
  `title_left_caps` no longer reserves a theme width.

## Open File's native picker (#2192)

- `Docxy::pick_open_file` (File > Open…, Ctrl+O, the Backstage Open rail, Open
  Read-Only…, Open as Copy…, Open and Repair…, Recover Text…) dispatches on
  `open_mode::open_pick_route()`. The arms have no `cfg`, so every OS compiles
  all three, and the route is the one place that decides:
  - **Linux, `Background`:** rfd's portal pick runs on the background executor,
    so the window keeps dispatching (Wayland pings). The result comes back
    through `update_in`.
  - **Windows, `Deferred`:** `IFileDialog::Show` runs a modal loop on the main
    thread, and gpui_windows runs foreground tasks from its messages. A
    listener runs from a window message with no task wake pending
    (`wake_posted` clear), so the first task dispatched while the picker is up
    posts a wake, and the picker's loop runs it with the App `RefCell` still
    borrowed. A task that updates an entity through `AsyncApp`
    (gpui-component's timers, for one) then panics with "RefCell already
    borrowed" and aborts in `window_procedure`. The dialog is therefore shown
    from a task spawned with `spawn_in`, which holds no borrow, as gpui's own
    `prompt_for_paths` does. It is owned by the suite window (`set_parent`),
    which is disabled while it is up. Never call `pick_file` before the spawn.
    Since #2194 this arm and macOS's go through `Docxy::native_dialog`, as
    every other native dialog does (below).
  - **macOS, `Inline`:** synchronous, under `native_modal`'s menu-bar swap.
    Not reported to crash, but **not verified safe**: gpui_macos runs
    foreground runnables on the main dispatch queue, which may drain in the
    panel's modal run loop (follow-up).
- While the Deferred picker is up, gpui's foreground tasks wait in every suite
  window. `run_foreground_task` sets `wake_posted` before it runs the picker's
  task, and `dispatch_on_main_thread` posts a wake only when that flag was
  clear. So timers, async results and AutoRecover stall until the picker
  closes. Painting goes on, since `WM_PAINT` is not a task.
- `set_parent` disables only its owner. Another suite window, such as a
  torn-off tab's (#1873), stays live, and an Open there queues its task. That
  picker opens after the first one closes and its `finish_open_file` has run,
  so pickers come one after another, in click order. The exception is
  dragging or resizing another suite window: its size/move-loop timer
  (`handle_timer_msg`) drains the queue, so queued tasks, a second picker
  among them, can run nested in the first one's loop. That is safe, since no
  borrow is held. If the owner closes first, `update_in` fails and the result
  is dropped.
- The unit test `open_picker_route_per_os` pins the route;
  `open-picker-windows.uit` runs the real Windows picker (#2195, below).

## Open's real picker in the Windows sweep (#2195)

- `pick_open_file` refuses Open in a harness with `OPEN_HARNESS` (moved there
  from Ctrl+F12, so the rail's Open… and the open modes refuse too) unless
  the route is `Deferred` (`open_mode::open_refused_in_harness`). macOS's
  Inline picker would block the control pump inside the verb. Linux's
  Background pick would not, but refusing there gives a deterministic
  message and no stray portal call under xvfb.
- While the Deferred picker is up no verb is answered (`wake_posted`,
  above), so uiharness answers it from its own process by window messages
  (`native-dialog`, `uiharness/src/native_dialog.rs`). It finds a visible
  `#32770` of the suite's pid owned by a window of that pid (`set_parent`),
  holds it 1.5 s, and cancels it or types a path and presses OK.
- ⚠️ A harness verb's input never reproduces #2192: the pump dispatches
  `real-key` and `pointer-click` from inside its own task, and
  `run_foreground_task` keeps `wake_posted` set while any runnable runs, so
  nothing nests even in an inline picker. The regression case clicks the
  rail's Open… with `window click` (posted `WM_MOUSEMOVE`, `WM_LBUTTONDOWN`,
  `WM_LBUTTONUP`), so its listener runs from a window message with the App
  borrowed and no wake pending. Ctrl+F12 stays `real-key`: a posted key has
  no Ctrl (`GetKeyState`) and `SendInput` cannot reach the sweep's desktop.
- `arm-task` is the crashing task on demand: a foreground ticker updating
  `Docxy` through `AsyncApp` (`harness_task_runs`). A ticker, not one shot:
  a cold first `IFileDialog::Show` can take over a second, and a task due
  before the picker shows runs harmlessly.
- The suite's own rail items draw probes (`bs-rail:<id without bs->`).
- The case order in `open-picker-windows.uit` matters: one suite runs the
  file, so the crash case goes after the Ctrl+F12 one.

## Every other native dialog (#2194)

- `Docxy::native_dialog` (`native_dialog.rs`) is the one way to show an rfd
  dialog: it takes the dialog, how to show it, and `then`, which is
  everything the listener did after the answer. `dialog_route()` decides:
  - **Windows, `Deferred`:** the same as Open's picker above. The dialog is
    owned by the window and shown from a `spawn_in` task, and `then` runs
    through `update_in` and is followed by `cx.notify()`. A task whose
    window has already gone shows nothing.
  - **Linux and macOS, `Inline`:** shown now, under `native_modal`, and
    `then` runs before `native_dialog` returns, as before. Linux's portal
    call blocks without running gpui's tasks.
  Nothing a caller does after `native_dialog` may depend on the answer. A
  `then` that acts on the active tab first checks that the tab is still the
  one that asked (`active_tab_is`). The window is disabled while the dialog is
  up, so this is defence. Sites: Save As for documents (`pick_doc_save_target`,
  for Save, Save As, Export's Change File Type and the close prompt's More
  options…), workbooks and Projects, the macro-free question, Project Export,
  header/footer Insert Picture, Mailings' Use an Existing List (taken in
  `mail_act`, which has the window) and the close warning about a draft that
  was not kept. `native_dialog::tests::no_native_dialog_outside_the_helper`
  fails on any `native_modal(` left elsewhere (Open's Linux Background arm is
  the one exception). The macOS scan counts a dialog as wrapped only inside
  `native_dialog(`'s `show` argument (the second), not in `dialog` or `then`.
- **A save is over when its dialogs are answered.** `save_active_inner`,
  `save_doc_then`, `save_sheet_then` and `save_project_then` take an
  `AfterSave` and hand it back (`Some`) when the save is over now. When a
  dialog's answer carries the save on, they move it into that answer
  (`None`); `#[must_use]` keeps a caller from dropping it.
  `save_active_then` returns nothing: it runs what `save_active_inner`
  hands back. ⚠️ It runs **last**, after the save's whole tail (the
  workbook's `circle_cmd_end`, `note_recent_if_moved`, the backstage
  resets, `persist`, `refocus`), since it may close the tab and shift every
  index. On the Deferred route it is dropped with the answer, never run,
  when the window goes before the dialog shows or while it is up.
- **The close prompt's Save and More options…** close the tab from that
  `AfterSave` (`close_prompt_saved`). It closes only when the tab is clean
  and its top dialog is still the prompt. Its `Result` (which the harness
  reads) is passed through an `Rc<Cell>`: the close's own result when it ran
  before `close_prompt_click` returned (always off Windows, and on Windows
  whenever no dialog asked), else `Ok(())`. A harness never asks.
  `close_tab_with`'s `CloseStep::Save` is reached only by a harness
  `close-tab save` (`debug_assert!`), so its save is over when it returns.
- **Workbook Save is steps** (`SheetSaveStep`): `sheet_save_begin`, then
  `sheet_save_answer` for each answer (`Pick`, the Save As dialog, then
  possibly `Confirm`, the macro-free question, then `Done`). The tests'
  `save_sheet_tab` loops over them with closures, and
  `Docxy::sheet_save_drive` drives them through `native_dialog`, so both run
  one sequence. The circle-warning bracket (#759) is begun before the first
  step and ended once, at `Done`, by the driver.
- No test runs a real Windows dialog. The crash fix is guarded by the source
  scan and the route tests (`dialog_route_per_os`, and that the two routes
  agree off Linux). The step tests guard the split of the workbook save.
